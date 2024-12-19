use anyhow::bail;
use anyhow::ensure;
use anyhow::Context;
use toml_edit::Array;
use toml_edit::ArrayOfTables;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::TableLike;
use toml_edit::Value;

const POST_TABLE: &str = "post";

enum ArrayOfTablesLike<'a> {
    Array(&'a Array),
    ArrayOfTables(&'a ArrayOfTables),
}

impl<'a> ArrayOfTablesLike<'a> {
    /// Iter over tables
    fn iter(&self) -> Box<dyn Iterator<Item = &'a dyn TableLike> + 'a> {
        match self {
            Self::Array(array) => Box::new(array.iter().map(|value| {
                value.as_inline_table().expect("value must be a table") as &dyn TableLike
            })),
            Self::ArrayOfTables(array) => {
                Box::new(array.iter().map(|table| table as &dyn TableLike))
            }
        }
    }

    /// Get the number of tables.
    pub fn len(&self) -> usize {
        match self {
            Self::Array(array) => array.len(),
            Self::ArrayOfTables(array) => array.len(),
        }
    }
}

/// The config for a file syncing.
#[derive(Debug)]
pub struct Config {
    document: DocumentMut,
}

impl Config {
    /// Make a config from a string.
    pub fn new(input: &str) -> anyhow::Result<Self> {
        let document: DocumentMut = input.parse()?;
        let post_table = document
            .as_table()
            .get(POST_TABLE)
            .context("missing \"post\" table")?
            .as_table_like()
            .context("\"post\" key does not refer to a table")?;
        let _id = post_table
            .get("id")
            .map(|item| {
                item.as_str()
                    .context("\"id\" field of post config is not a string")
            })
            .transpose()?;
        let _title = post_table
            .get("title")
            .map(|item| {
                item.as_str()
                    .context("\"title\" field of post config is not a string")
            })
            .transpose()?;
        let _privacy = post_table
            .get("privacy")
            .map(|item| {
                item.as_str()
                    .context("\"privacy\" field of post config is not a string")?
                    .parse::<PostConfigPrivacy>()
                    .context("failed to parse post privacy")
            })
            .transpose()?;
        let _nsfw = post_table
            .get("nsfw")
            .map(|item| {
                item.as_bool()
                    .context("\"nsfw\" field of post config is not a bool")
            })
            .transpose()?;
        let files = {
            let item = post_table
                .get("files")
                .context("missing \"files\" key of post config")?;

            match item {
                Item::Value(Value::Array(array)) => {
                    for value in array.iter() {
                        ensure!(
                            value.is_inline_table(),
                            "\"files\" field of post config must be an array of tables"
                        );
                    }

                    ArrayOfTablesLike::Array(array)
                }
                Item::ArrayOfTables(array) => ArrayOfTablesLike::ArrayOfTables(array),
                _ => {
                    bail!("\"files\" key of post config is not an array of tables");
                }
            }
        };
        ensure!(
            files.len() != 0,
            "\"files\" array of post config must have at least one entry"
        );
        for (i, table) in files.iter().enumerate() {
            let file_n = i + 1;

            let _path = table
                .get("path")
                .with_context(|| format!("file {file_n} of post config missing \"path\""))?
                .as_str()
                .with_context(|| {
                    format!("file {file_n} of post config \"path\" key is not a string")
                });
            let _description = table
                .get("description")
                .map(|item| {
                    item.as_str().with_context(|| {
                        format!("file {file_n} of post config \"description\" key is not a string")
                    })
                })
                .transpose()?;
        }

        Ok(Self { document })
    }

    /// Get the post config mutably.
    pub fn post_mut(&mut self) -> PostConfig {
        let table = self
            .document
            .as_table_mut()
            .get_mut(POST_TABLE)
            .expect("missing \"post\" table")
            .as_table_like_mut()
            .expect("\"post\" key does not refer to a table");

        PostConfig { table }
    }
}

impl std::fmt::Display for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.document.fmt(f)
    }
}

/// The post config.
pub struct PostConfig<'a> {
    table: &'a mut dyn TableLike,
}

impl PostConfig<'_> {
    /// Get the id
    pub fn id(&self) -> Option<&str> {
        self.table.get("id").map(|item| {
            item.as_str()
                .expect("\"id\" field of post config is not a string")
        })
    }

    /// Set the id.
    pub fn set_id(&mut self, id: Option<&str>) {
        let id = match id {
            Some(id) => id,
            None => {
                self.table.remove("id");
                return;
            }
        };

        // The toml_edit library,
        // the library meant for editing toml,
        // has absolutely no way to specify where an inserted key goes.
        //
        // Additionally, the library's abstract table interface is incomplete,
        // not allowing a custom comparator for sort.
        // This means that it is impossible to choose where this insert will go.
        self.table.insert("id", toml_edit::value(id));
    }

    /// Get the title.
    pub fn title(&self) -> Option<&str> {
        self.table.get("title").map(|item| {
            item.as_str()
                .expect("\"title\" field of post config is not a string")
        })
    }

    /// Get the privacy.
    pub fn privacy(&self) -> Option<PostConfigPrivacy> {
        self.table.get("privacy").map(|item| {
            item.as_str()
                .expect("\"privacy\" field of post config is not a string")
                .parse::<PostConfigPrivacy>()
                .expect("failed to parse post privacy")
        })
    }

    /// Get the nsfw.
    pub fn nsfw(&self) -> Option<bool> {
        self.table.get("nsfw").map(|item| {
            item.as_bool()
                .expect("\"nsfw\" field of post config is not a bool")
        })
    }

    /// Iter over the files.
    pub fn files(&self) -> PostConfigFilesArray {
        let item = self
            .table
            .get("files")
            .expect("missing \"files\" key of post config");

        let array = match item {
            Item::Value(Value::Array(array)) => {
                for value in array.iter() {
                    if value.is_inline_table() {
                        panic!("\"files\" field of post config must be an array of tables");
                    }
                }

                ArrayOfTablesLike::Array(array)
            }
            Item::ArrayOfTables(array) => ArrayOfTablesLike::ArrayOfTables(array),
            _ => {
                panic!("\"files\" key of post config is not an array of tables");
            }
        };

        PostConfigFilesArray { array }
    }
}

/// Config for the post files array.
pub struct PostConfigFilesArray<'a> {
    array: ArrayOfTablesLike<'a>,
}

impl PostConfigFilesArray<'_> {
    /// Iter over files.
    pub fn iter(&self) -> impl Iterator<Item = PostConfigFile> {
        self.array.iter().map(|table| PostConfigFile { table })
    }

    /// Get the number of files.
    pub fn len(&self) -> usize {
        self.array.len()
    }
}

/// A post config file.
pub struct PostConfigFile<'a> {
    table: &'a dyn TableLike,
}

impl PostConfigFile<'_> {
    /// The file path.
    pub fn path(&self) -> &str {
        self.table
            .get("path")
            .expect("missing path")
            .as_str()
            .expect("path is not a str")
    }

    /// The file description
    pub fn description(&self) -> Option<&str> {
        self.table
            .get("description")
            .map(|item| item.as_str().expect("description is not a str"))
    }
}

/// Post privacy
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PostConfigPrivacy {
    Public,
    Hidden,
    Secret,
}

impl std::str::FromStr for PostConfigPrivacy {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "public" => Ok(Self::Public),
            "hidden" => Ok(Self::Hidden),
            "secret" => Ok(Self::Secret),
            _ => bail!("\"{input}\" is not a valid post privacy type"),
        }
    }
}

/// Config for a user.
#[derive(Debug)]
pub struct UserConfig {
    document: DocumentMut,
}

impl UserConfig {
    /// Make a config from a string.
    pub fn new(input: &str) -> anyhow::Result<Self> {
        let document: DocumentMut = input.parse()?;
        let _token = document
            .get("token")
            .map(|item| {
                item.as_str()
                    .context("\"token\" field of user config is not a string")
            })
            .transpose()?;

        Ok(Self { document })
    }

    /// Get the token, if it exists.
    pub fn token(&self) -> Option<&str> {
        self.document.get("token").map(|item| {
            item.as_str()
                .expect("\"token\" field of user config is not a string")
        })
    }

    /// Set the token.
    ///
    /// If the empty string is passed, the token key is deleted.
    pub fn set_token(&mut self, new_token: &str) {
        if new_token.is_empty() {
            self.document.remove("token");
        }

        self.document.insert("token", toml_edit::value(new_token));
    }
}

impl std::fmt::Display for UserConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.document.fmt(f)
    }
}
