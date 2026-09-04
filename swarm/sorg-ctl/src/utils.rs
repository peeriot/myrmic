use std::fmt::Display;

use comfy_table::{Cell, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};

pub(crate) const MAX_WIDTH_ID: usize = 8;

#[macro_export]
macro_rules! print_success {
    ($($arg:tt)*) => {
        {use crossterm::style::Stylize;
            println!("\n✅ {} ✅", "SUCCESS".green());
        println!($($arg)*)}
    };
}

#[macro_export]
macro_rules! print_info {
    ($($arg:tt)*) => {
        {use crossterm::style::Stylize;
            println!("\nℹ️ {} ℹ️", "INFO".cyan());
        println!($($arg)*)}
    };
}

#[macro_export]
macro_rules! print_error {
    ($($arg:tt)*) => {
        {use crossterm::style::Stylize;
            println!("\n❌ {} ❌", "ERROR".red().slow_blink());
        println!($($arg)*)}
    };
}

pub(crate) fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS);
    table
}

pub(crate) trait AsCell {
    fn cell(&self) -> Cell;

    fn cell_prefix(&self, width: usize) -> Cell;
}

impl<T> AsCell for T
where
    T: Display,
{
    fn cell(&self) -> Cell {
        Cell::new(self.to_string())
    }

    fn cell_prefix(&self, width: usize) -> Cell {
        let content = self.to_string();
        if content.len() <= width {
            Cell::new(content)
        } else {
            let prefix = content.chars().take(width - 3).collect::<String>();
            Cell::new(format!("{prefix}..."))
        }
    }
}
