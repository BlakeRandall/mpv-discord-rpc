fn main() {
    println!("cargo:rerun-if-changed=.env");

    // Load .env if available. Existing environment variables take precedence.
    dotenvy::dotenv().ok();

    let app_id = std::env::var("DISCORD_APP_ID")
        .expect("DISCORD_APP_ID must be set in .env or the build environment");

    println!("cargo:rustc-env=DISCORD_APP_ID={}", app_id);
}