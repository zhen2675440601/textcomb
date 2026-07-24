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
    /// Run all database migrations.
    Migrate,
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
        Commands::Migrate => {
            db::migrate(&pool).await?;
            println!("Database migrations completed.");
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
            let pdftotext = command_available("pdftotext");
            let typst = command_available("typst");
            println!("database: ok");
            println!("pdftotext: {}", if pdftotext { "ok" } else { "missing" });
            println!("typst: {}", if typst { "ok" } else { "missing" });
            if !pdftotext || !typst {
                anyhow::bail!("required binary is missing");
            }
        }
    }
    Ok(())
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}
