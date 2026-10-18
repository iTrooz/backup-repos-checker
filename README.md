backup-repos-checker - A tool to monitor that your backups are always being done, and valid.
====

# What is this ?

This project is a Rust CLI that monitors a folder tree, more particularly "repository folders" that are maintained by external tools, e.g. restic, to check they are "valid". The list of checks include:
- Ensuring at least one file in the repository was modified during the last X days (to ensure the backup client did not go down)
- Ensuring all files in the repository match a pattern (to ensure you didn't include unencrypted files)
- Ensuring all folders in the tree are tracked, by being part of a repository

Check violations are displayed to the terminal when using `backup-repos-checker check` and sent by email when using `backup-repos-checker report`.

For now, this tool only supports reading the folder tree from S3, because this is what I use for my backups, but I'm open to adding support for other storage providers.
