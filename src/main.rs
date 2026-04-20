use std::path::PathBuf;

use porchetta::rooted_tree::RootedTree;

fn main() {
    println!(
        "{:#?}",
        RootedTree::capture(
            PathBuf::from("."),
            |_, bs| Ok(bs),
            |p| !p
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == "target")
        )
    );
}
