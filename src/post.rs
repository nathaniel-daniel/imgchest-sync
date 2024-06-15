use camino::Utf8PathBuf;

/// Representation of a post.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Post {
    /// The title
    pub title: String,

    /// The privacy of this post.
    pub privacy: PostPrivacy,

    /// Whether this post is nsfw.
    pub nsfw: bool,

    /// The post files
    pub files: Vec<PostFile>,
}

/// The post privacy.
#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum PostPrivacy {
    /// The post is public
    #[serde(rename = "public")]
    Public,

    /// The post is hidden
    #[serde(rename = "hidden")]
    Hidden,

    /// The post is secret
    #[serde(rename = "secret")]
    Secret,
}

/// A post image
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PostFile {
    /// The post file description.
    pub description: String,

    /// The sha256 file hash, as a hex string.
    pub sha256: String,

    /// The post path.
    ///
    /// This may not exist in certain cases,
    /// like loading a cache from the disk.
    ///
    /// This should not be used when diffing.
    pub path: Option<Utf8PathBuf>,

    /// The post id
    ///
    /// This may not exist in certain cases,
    /// like creating a post from a config file.
    ///
    /// This should not be used when diffing.
    pub id: Option<String>,
}

/// A diff for a post.
#[derive(Debug, PartialEq)]
pub enum PostDiff {
    EditTitle {
        /// The new title.
        title: String,
    },
    EditPrivacy {
        /// The new privacy setting.
        privacy: PostPrivacy,
    },
    EditNsfw {
        /// The new nsfw setting.
        nsfw: bool,
    },
    RetainFile {
        /// The index of the file to retain.
        index: usize,
    },
    AddFile {
        /// The index of the new post.
        index: usize,
    },
    RemoveFile {
        /// The index of the file to remove.
        index: usize,
    },
}
