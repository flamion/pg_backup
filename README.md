# pg_backup

Command-line utility to back up PostgreSQL databases with optional zstd compression and nice defaults :3

## Features
- Back up a specific database or all non-system databases in one run.
- Optional zstd compression (level 18 by default, uses all available cores).
- Output directory validation to avoid mixing non-backup files.

## Requirements
- PostgreSQL client tools: `psql`, `pg_dump`
- `zstd` (only when using `--compress`)

## Quick start
1. Build the binary with your preferred toolchain.
2. Run the helper or a backup command:
   - `pg_backup --help`
   - `pg_backup --output-dir ./pg_backups`
   - `pg_backup --database mydb --compress --host db.example.com --port 5432 --username backup_user`

## Behavior
- Backup files are named `<database>-YYYY-MM-DD_HH-mm-ss.sql` (or `.sql.zst` when compressed) inside the chosen output directory.
- Exits with a non-zero status if any backup fails and prints a summary of successes and failures.
