//! `rust-embed`'s derive macro needs `web/dist` to exist at compile time; on a
//! fresh checkout (no `pnpm build` run yet) it's absent, and the macro fails
//! to generate the `Embed` impl at all (E0599: no `get` on `Assets`) rather
//! than embedding zero files. Create it if missing so `cargo build`/`clippy`/
//! `test` work before the frontend is built; a real `pnpm build` overwrites
//! it with the actual assets.

fn main() {
    std::fs::create_dir_all("web/dist").expect("create web/dist placeholder for rust-embed");
}
