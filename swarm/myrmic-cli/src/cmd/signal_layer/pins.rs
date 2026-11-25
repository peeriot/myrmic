//! The example bus pins and usable-GPIO list a firmware pipeline's board
//! manifest is scaffolded with.

use anyhow::Context as _;

/// The example i2c bus pins for `chip` and, rendered as the board manifest's
/// `general_purpose` list, every other usable GPIO.
pub(super) struct BoardPins {
    pub(super) scl: u8,
    pub(super) sda: u8,
    pub(super) general_purpose: String,
}

pub(super) fn board_pins(chip: &str) -> anyhow::Result<BoardPins> {
    let usable = esp_codegen::chip_general_purpose_pins(chip)
        .with_context(|| format!("no GPIO layout for chip `{chip}`"))?;
    let (scl, sda) = pick_bus_pins(chip, &usable)?;
    let general_purpose = usable
        .iter()
        .copied()
        .filter(|pin| *pin != scl && *pin != sda)
        .map(|pin| pin.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(BoardPins {
        scl,
        sda,
        general_purpose,
    })
}

/// Picks the example i2c bus pins: the conventional devkit pins for the chip
/// when both are usable, else the first two usable pins.
fn pick_bus_pins(chip: &str, usable: &[u8]) -> anyhow::Result<(u8, u8)> {
    let conventional = match chip {
        "esp32c6" => Some((10u8, 11u8)),
        "esp32c5" | "esp32c61" => Some((23u8, 24u8)),
        _ => None,
    };
    if let Some((scl, sda)) = conventional
        && usable.contains(&scl)
        && usable.contains(&sda)
    {
        return Ok((scl, sda));
    }
    let mut it = usable.iter().copied();
    let scl = it.next().context("chip has no usable GPIOs")?;
    let sda = it
        .next()
        .context("chip needs at least two usable GPIOs for the example bus")?;
    Ok((scl, sda))
}
