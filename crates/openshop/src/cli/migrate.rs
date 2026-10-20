//! `migrate`, `migrate generate`, `migrate check` and `migrate status`.

use anyhow::{Result, bail};

use super::database_url;
use crate::app::OpenShopConfig;
use crate::db::migrate::{
    MigrationStatus, check_migrations, generate_migrations, get_client_migration_status, migrate_schema,
};
use crate::runtime::RuntimeEnv;

fn print_status(status: &MigrationStatus) {
    println!("Client migrations:");
    let Some(folder) = &status.folder else {
        println!("  folder: none");
        println!("  applied: 0");
        println!("  pending: 0");
        return;
    };

    let list = |tags: &[String]| {
        if tags.is_empty() {
            String::new()
        } else {
            format!(" ({})", tags.join(", "))
        }
    };
    println!("  folder: {}", folder.display());
    println!("  applied: {}{}", status.applied.len(), list(&status.applied));
    println!("  pending: {}{}", status.pending.len(), list(&status.pending));
}

pub async fn run_migrate(config: &OpenShopConfig, args: &[String]) -> Result<()> {
    let env = RuntimeEnv::from_env();
    let cwd = env.cwd.as_path();

    match args.first().map(String::as_str) {
        Some("generate") => {
            let generated = generate_migrations(cwd, &args[1..], env.production, &config.models)?;
            if generated.files.is_empty() && generated.snapshot.is_none() && generated.warnings.is_empty() {
                println!("[openshop] No schema changes, nothing to generate");
            }
            for path in &generated.files {
                println!("[openshop] Generated {}", path.display());
            }
            if let Some(snapshot) = &generated.snapshot {
                println!("[openshop] Updated {}", snapshot.display());
            }
            for warning in &generated.warnings {
                println!("[openshop] Review: {warning}");
            }
            Ok(())
        }
        Some("check") => {
            check_migrations(cwd, &config.models)?;
            println!("[openshop] Migrations cover the current schema");
            Ok(())
        }
        Some("status") => {
            let pool = crate::db::connect_to(&database_url()).await?;
            print_status(&get_client_migration_status(&pool, cwd).await?);
            Ok(())
        }
        None => {
            let pool = crate::db::connect_to(&database_url()).await?;
            let applied = migrate_schema(&pool, cwd).await?;
            for tag in &applied {
                println!("[openshop] Applied {tag}");
            }
            println!("[openshop] Client migrations applied");
            Ok(())
        }
        Some(other) => bail!("[openshop] Unknown migrate command: {other}"),
    }
}
