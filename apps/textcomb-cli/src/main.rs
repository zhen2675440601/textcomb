use clap::{Parser, Subcommand};
use secrecy::SecretString;
use std::process::Command;
use textcomb_core::{AppConfig, auth, db};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "textcomb-cli", version, about = "TextComb administration")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run migrations and optionally initialize an administrator on an empty database.
    Migrate {
        #[arg(long, env = "TEXTCOMB_ADMIN_USERNAME", default_value = "admin")]
        admin_username: String,
        #[arg(long, env = "TEXTCOMB_ADMIN_PASSWORD", hide_env_values = true)]
        admin_password: Option<String>,
    },
    /// Create the first super administrator. Only works on an empty user table.
    BootstrapAdmin {
        #[arg(long)]
        username: String,
        #[arg(long, env = "TEXTCOMB_ADMIN_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Create a regular user from the trusted local CLI.
    CreateUser {
        #[arg(long)]
        username: String,
        #[arg(long, env = "TEXTCOMB_USER_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Reset a user's password and revoke all sessions.
    ResetPassword {
        #[arg(long)]
        user_id: Uuid,
        #[arg(long, env = "TEXTCOMB_USER_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Check database and required document/report binaries.
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let config = AppConfig::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    match cli.command {
        Commands::Migrate {
            admin_username,
            admin_password,
        } => {
            db::migrate(&pool).await?;
            println!("Database migrations completed.");
            if let Some(password) = admin_password.filter(|value| !value.is_empty()) {
                match auth::bootstrap_admin_if_empty(
                    &pool,
                    &admin_username,
                    &SecretString::from(password),
                )
                .await?
                {
                    Some(user) => println!(
                        "Created super administrator {} ({})",
                        user.username, user.id
                    ),
                    None => {
                        println!("Existing users preserved; administrator initialization skipped.")
                    }
                }
            } else {
                println!(
                    "Administrator initialization skipped: TEXTCOMB_ADMIN_PASSWORD is not set."
                );
            }
        }
        Commands::BootstrapAdmin { username, password } => {
            db::migrate(&pool).await?;
            let user =
                auth::bootstrap_admin(&pool, &username, &SecretString::from(password)).await?;
            println!(
                "Created super administrator {} ({})",
                user.username, user.id
            );
        }
        Commands::CreateUser { username, password } => {
            db::migrate(&pool).await?;
            let user =
                auth::create_user(&pool, &username, &SecretString::from(password), "user").await?;
            println!("Created user {} ({})", user.username, user.id);
        }
        Commands::ResetPassword { user_id, password } => {
            auth::reset_password(&pool, user_id, &SecretString::from(password)).await?;
            println!("Password reset; active sessions revoked.");
        }
        Commands::Doctor => {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(&pool)
                .await?;
            let pdftotext = command_available("pdftotext", "-v");
            let pdfinfo = command_available("pdfinfo", "-v");
            let pdftocairo = command_available("pdftocairo", "-v");
            let pdfimages = command_available("pdfimages", "-v");
            let typst = command_available("typst", "--version");
            println!("database: ok");
            println!("pdftotext: {}", if pdftotext { "ok" } else { "missing" });
            println!("pdfinfo: {}", if pdfinfo { "ok" } else { "missing" });
            println!("pdftocairo: {}", if pdftocairo { "ok" } else { "missing" });
            println!("pdfimages: {}", if pdfimages { "ok" } else { "missing" });
            println!("typst: {}", if typst { "ok" } else { "missing" });
            if !pdftotext || !pdfinfo || !pdftocairo || !pdfimages || !typst {
                anyhow::bail!("required binary is missing");
            }
        }
    }
    Ok(())
}

fn command_available(command: &str, version_flag: &str) -> bool {
    Command::new(command)
        .arg(version_flag)
        .output()
        .is_ok_and(|output| output.status.success())
}
