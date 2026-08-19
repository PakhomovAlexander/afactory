//! Regenerate a lockfile: `cargo run -p review-config --example lock -- <registry> <name>...`
//!
//! Prints the lock TOML for the named packages as they stand in the registry. This is the
//! *only* way a pin is meant to be produced — a hand-typed digest pins whatever was typed, not
//! what is on disk.

use review_config::lock::{Lockfile, Registry};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: lock <registry-dir> <package-name>...");
        std::process::exit(2);
    };
    let registry = Registry::new([root]);
    let mut lockfile = Lockfile::empty();
    for name in args {
        match Lockfile::pin(&name, &registry) {
            Ok(pin) => {
                lockfile.reviewers.insert(name, pin);
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    print!("{}", lockfile.to_toml());
}
