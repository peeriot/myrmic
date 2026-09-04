//! Table-rendering helpers shared by the status commands: column widths and
//! id shortening with a highlighted unique prefix.

pub const BOLD: &str = "\x1b[1m";
pub const BOLD_CYAN: &str = "\x1b[1;36m";
pub const DIMMED: &str = "\x1b[2m";
pub const RESET: &str = "\x1b[0m";

/// How much of an id to show; a longer unique prefix extends it.
pub const ID_CHARS: usize = 8;

/// Stands in for a column a row has no value for.
pub const NONE: &str = "—";

pub fn width<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(|c| c.chars().count()).max().unwrap_or(0)
}

/// A cell fit to print: control characters dropped and the rest capped at `max`
/// characters.
///
/// Names, kinds and tags are all self-reported by whoever is on the network, so
/// none of them can be trusted to be printable or short. Left as-is, a newline
/// tears the table apart, an ANSI escape runs in the reader's terminal, and one
/// long value widens a column for every other row.
pub fn cell(value: &str, max: usize) -> String {
    let printable: String = value.chars().filter(|c| !c.is_control()).collect();
    if printable.chars().count() <= max {
        return printable;
    }

    printable
        .chars()
        .take(max.saturating_sub(1))
        .chain(['…'])
        .collect()
}

/// For each id, the shortest prefix length that no other id shares (min 1).
pub fn unique_prefix_lengths(ids: &[String]) -> Vec<usize> {
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let longest_shared = ids
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, other)| common_prefix_len(id, other))
                .max()
                .unwrap_or(0);
            (longest_shared + 1).min(id.chars().count()).max(1)
        })
        .collect()
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// An id shortened to [`ID_CHARS`] (or its unique prefix, whichever is longer)
/// with that prefix highlighted — ANSI when styled, `[..]` otherwise. Returns
/// the rendered cell and the width it occupies on screen.
pub fn styled_id(id: &str, uniq_len: usize, styled: bool) -> (String, usize) {
    let chars: Vec<char> = id.chars().collect();
    let shown = uniq_len.max(ID_CHARS).min(chars.len());
    let split = uniq_len.min(shown);
    let prefix: String = chars[..split].iter().collect();
    let rest: String = chars[split..shown].iter().collect();
    if styled {
        (
            format!("{BOLD_CYAN}{prefix}{RESET}{DIMMED}{rest}{RESET}"),
            shown,
        )
    } else {
        (format!("[{prefix}]{rest}"), shown + 2)
    }
}
