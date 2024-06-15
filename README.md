# imgchest-sync
A CLI to upload and sync folders of images to https://imgchest.com.

## Installation
1. [Install Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html)
2. Run:
```bash
cargo install --git https://github.com/nathaniel-daniel/imgchest-sync
```

## Usage
You need an api token to use this program.
It is used from a terminal like so:
```bash
imgchest-sync --token <TOKEN> --input <input_directory>
```

### Post Config
Inside each folder you wish to sync, create a file called `imgchest-sync.toml`.
This file has the following format:
```toml
[post]
# The post id you want to sync to.
# This is optional, and will be populated automatically if you choose to omit it.
id = "<the post id>"

# The title of the post.
# It must be more than 3 characters. 
# This is optional.
title = "<the title>"

# The privacy of the post.
# It is optional, and defaults to "hidden".
# Valid values are: "public", "hidden", "secret"
privacy = "<the post privacy>"

# The nsfw flag of the post.
# It is optional, and defaults to false.
# Valid values are: true, false
nsfw = false

# This is an array of images to upload.
# You are required to have at least one.
[[post.files]]
# This is the path to the file to upload.
# This is required.
path = "<path to file>"

[[post.files]]
path = "<path to file>"
```

## License
Licensed under either of
 * Apache License, Version 2.0 (LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license (LICENSE-MIT or http://opensource.org/licenses/MIT)
at your option.

## Contributing
Unless you explicitly state otherwise, 
any contribution intentionally submitted for inclusion in the work by you, 
as defined in the Apache-2.0 license, 
shall be dual licensed as above, 
without any additional terms or conditions.