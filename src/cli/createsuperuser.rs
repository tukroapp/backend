use std::io::{Read, Write};

use anyhow::Context;
use sqlx::PgPool;

use crate::{
    configuration::Settings, constants::PASSWORD_MIN_LENGTH, services::slug::create_user_profile,
    startup::get_connection_pool, utils::compute_hash,
};

pub async fn run(
    configuration: &Settings,
    email: Option<String>,
    password_stdin: bool,
    skip_password_validation: bool,
) -> Result<(), anyhow::Error> {
    let email = match email {
        Some(email) => email,
        None => prompt("E-mail: ")?,
    };

    if email.is_empty() {
        anyhow::bail!("O e-mail é obrigatório.");
    }

    let password = if password_stdin {
        read_password_from_stdin()?
    } else {
        ask_password()?
    };

    if !skip_password_validation {
        validate_password(&password)?;
    }

    let password_hash = compute_hash(password).context("Failed to hash the password")?;
    let pool = get_connection_pool(&configuration.database);

    create_superuser(&pool, &email, &password_hash).await?;

    println!("Administrador {email} criado.");

    Ok(())
}

async fn create_superuser(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<(), anyhow::Error> {
    let mut tx = pool
        .begin()
        .await
        .context("Failed to connect to Postgres")?;

    let created = sqlx::query_scalar!(
        r#"
        INSERT INTO auth_user (email, password_hash, is_superuser, is_email_verified)
        VALUES ($1, $2, TRUE, TRUE)
        RETURNING id
        "#,
        email,
        password_hash,
    )
    .fetch_one(&mut *tx)
    .await;

    let user_id = match created {
        Ok(user_id) => user_id,
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            anyhow::bail!("Já existe um usuário com o e-mail {email}.");
        }
        Err(e) => return Err(anyhow::Error::new(e).context("Failed to create the administrator")),
    };

    create_user_profile(&mut tx, &user_id, "Administrador")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create the administrator profile: {e}"))?;

    tx.commit().await.context("Failed to commit transaction")?;

    Ok(())
}

/// A mesma regra do cadastro por e-mail.
fn validate_password(password: &str) -> Result<(), anyhow::Error> {
    if (password.chars().count() as u64) < PASSWORD_MIN_LENGTH {
        anyhow::bail!("A senha deve ter pelo menos {PASSWORD_MIN_LENGTH} caracteres.");
    }

    Ok(())
}

fn prompt(label: &str) -> Result<String, anyhow::Error> {
    print!("{label}");
    std::io::stdout()
        .flush()
        .context("Failed to write to stdout")?;

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("Failed to read from stdin")?;

    Ok(answer.trim().to_string())
}

fn ask_password() -> Result<String, anyhow::Error> {
    let password = rpassword::prompt_password("Senha: ").context("Failed to read the password")?;
    let confirmation =
        rpassword::prompt_password("Confirme a senha: ").context("Failed to read the password")?;

    if password != confirmation {
        anyhow::bail!("As senhas não conferem.");
    }

    Ok(password)
}

/// Modo não interativo: a senha é tudo que chega pela entrada padrão, menos a
/// quebra de linha final que o shell costuma mandar junto.
fn read_password_from_stdin() -> Result<String, anyhow::Error> {
    let mut password = String::new();
    std::io::stdin()
        .read_to_string(&mut password)
        .context("Failed to read the password from stdin")?;

    let password = password
        .strip_suffix("\r\n")
        .or_else(|| password.strip_suffix('\n'))
        .unwrap_or(&password);

    if password.is_empty() {
        anyhow::bail!("A senha não pode ser vazia.");
    }

    Ok(password.to_string())
}
