backup-repos-checker - A tool to monitor that your backups are always being done, and valid.
====

# What is this ?

This project is a Rust CLI that monitors a folder tree, more particularly "repository folders" that are maintained by external tools, e.g. restic, to check they are "valid". The list of checks include:
- Ensuring at least one file in the repository was modified during the last X days (to ensure the backup client did not go down)
- Ensuring all files in the repository match a pattern (to ensure you didn't include unencrypted files)
- Ensuring a repository does not exceed a configured maximum size (optional, per repository)
- Ensuring all folders in the tree are tracked, by being part of a repository

Check violations are displayed to the terminal when using `backup-repos-checker check` and sent by email when using `backup-repos-checker report`.

For now, this tool only supports reading the folder tree from S3, because this is what I use for my backups, but I'm open to adding support for other storage providers.

# Usage
Before running this tool, you need to fill its configuration (see [example](./config.example.toml)). You need to register all the repositories paths you want the tool to track, and potentially configure some checks, e.g. the content one. Also fill out the email and S3 server configuration.
Credentials need to be inputted through environment variables, see `.env.example` for an example of environment variables recognized.

# Deployment

A Dockerfile and compose file are provided to run the tool on a schedule. To use it:
- Copy `.env.example` to `.env` and edit it
- Copy `.config.example.toml` to `config.toml` and edit it
- `docker compose up --build -d`
- Done! You may run `backup-repos-checker report --dry-run` to ensure your S3 and email configuration is correct.

# Licence


[GPL-2.0-only OR GPL-3.0-or-later with proxy being iTrooz](https://itrooz.fr/posts/gpl_licence/)
