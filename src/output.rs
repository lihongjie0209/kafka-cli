//! Human-readable and machine-readable output helpers.

use std::io::{self, Write};

use comfy_table::{Table, presets::UTF8_FULL};
use serde::Serialize;

use crate::error::Result;

/// Supported output encodings.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum OutputFormat {
    /// Aligned human-readable text.
    #[default]
    Table,
    /// Stable JSON envelope.
    Json,
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    schema_version: u8,
    command: &'a str,
    data: T,
    errors: Vec<String>,
}

/// Writes a serializable value using the requested format.
pub fn write_value<T: Serialize>(
    format: OutputFormat,
    command: &str,
    value: &T,
    table: impl FnOnce(&T) -> String,
) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match format {
        OutputFormat::Table => writeln!(stdout, "{}", table(value))?,
        OutputFormat::Json => serde_json::to_writer_pretty(
            &mut stdout,
            &Envelope {
                schema_version: 1,
                command,
                data: value,
                errors: Vec::new(),
            },
        )?,
    }
    if format == OutputFormat::Json {
        writeln!(stdout)?;
    }
    Ok(())
}

/// Writes a single JSON Lines record.
pub fn write_json_line<T: Serialize>(value: &T) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

/// Renders rows with a consistent UTF-8 table style.
#[must_use]
pub fn table<const COLUMNS: usize>(
    headers: [&str; COLUMNS],
    rows: impl IntoIterator<Item = [String; COLUMNS]>,
) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL).set_header(headers);
    for row in rows {
        table.add_row(row);
    }
    table.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_should_render_headers_and_aligned_rows() {
        let rendered = table(
            ["TOPIC", "PARTITION"],
            [["events".into(), "0".into()], ["audit".into(), "12".into()]],
        );

        assert_eq!(
            rendered,
            "┌────────┬───────────┐\n│ TOPIC  ┆ PARTITION │\n╞════════╪═══════════╡\n│ events ┆ 0         │\n├╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌┤\n│ audit  ┆ 12        │\n└────────┴───────────┘"
        );
    }
}
