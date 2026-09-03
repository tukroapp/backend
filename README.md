# Tukro backend

Backend em Rust (Actix-web + SQLx + PostgreSQL + Redis) do Tukro.

## Configuração

Copie o `.env.example` para `.env` e preencha as variáveis. Toda a configuração
vem do ambiente e é lida uma única vez, seja qual for o subcomando.

## CLI

O crate expõe um único binário, `tukro`, com subcomandos. Em desenvolvimento,
rode com `cargo run --bin tukro -- <subcomando>`; em produção, o binário é
autossuficiente (as migrations vão embutidas nele, sem precisar dos arquivos
`.sql` no servidor).

```
tukro serve             # sobe o servidor HTTP
tukro migrate           # aplica as migrations pendentes
tukro migrate-info      # lista as migrations aplicadas e as pendentes
tukro health            # ping no Postgres e no Redis
tukro createsuperuser   # cria um administrador
```

### serve

Sobe o servidor HTTP. `--host` e `--port` sobrescrevem `APP_HOST` e `APP_PORT`.

```sh
tukro serve
tukro serve --host 127.0.0.1 --port 8123
```

### migrate

Aplica as migrations pendentes. Com `--dry-run`, só lista o que seria aplicado.

```sh
tukro migrate
tukro migrate --dry-run
```

### migrate-info

Lista todas as migrations com versão, descrição e situação.

```sh
tukro migrate-info
# aplicada 20260514180020 initial
```

### health

Abre conexão com o Postgres e com o Redis, faz um ping em cada um e sai com 1 se
algum falhar. Serve para conferir a configuração de um servidor sem subir o
processo.

```sh
tukro health
# postgres: ok
# redis: ok
```

### createsuperuser

Cria um usuário administrador. O e-mail vem por `--email` ou é perguntado no
terminal; a senha é lida sem echo, com confirmação, e segue as mesmas regras do
cadastro, a menos que `--skip-password-validation` peça o contrário. É a única
forma de criar um administrador.

```sh
tukro createsuperuser
tukro createsuperuser --email admin@tukro.app

# Sem terminal interativo (deploy, script): a senha vem da entrada padrão, sem
# confirmação.
echo 'senha-do-administrador' | tukro createsuperuser --email admin@tukro.app --password-stdin

# Aceita qualquer senha não vazia, ignorando o tamanho mínimo.
tukro createsuperuser --email admin@tukro.app --skip-password-validation
```

## Testes

```sh
cargo test
```
