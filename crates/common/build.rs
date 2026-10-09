// `sqlx::migrate!` embeds every file in db/migrations at compile time, but a
// newly added migration file is not tracked by the proc macro, so cached
// builds (locally and in Docker) would silently ship without it. Re-run the
// build whenever the directory changes.
fn main() {
    println!("cargo:rerun-if-changed=../../db/migrations");
}
