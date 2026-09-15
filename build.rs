//! The CLI carries its version (`build/version.rs`), which `\v` prints next to the engine's.

include!("build/version.rs");

fn main() {
    stamp_version();
}
