use super::*;

pub(crate) fn default_terminal_palette() -> PaletteState {
    PaletteState {
        default_fg: TerminalColor::Rgb { r: 0, g: 0, b: 0 },
        default_bg: TerminalColor::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        cursor: TerminalColor::Rgb { r: 0, g: 0, b: 0 },
        ansi: (0..16).map(xterm_indexed_color).collect(),
    }
}

pub(crate) fn dark_terminal_palette() -> PaletteState {
    PaletteState {
        default_fg: TerminalColor::Rgb {
            r: 238,
            g: 238,
            b: 238,
        },
        default_bg: TerminalColor::Rgb {
            r: 17,
            g: 17,
            b: 17,
        },
        cursor: TerminalColor::Rgb {
            r: 238,
            g: 238,
            b: 238,
        },
        ansi: (0..16).map(xterm_indexed_color).collect(),
    }
}

pub(crate) fn xterm_indexed_color(index: u16) -> TerminalColor {
    const ANSI16: [(u8, u8, u8); 16] = [
        (0x0c, 0x0c, 0x0c),
        (0xc5, 0x0f, 0x1f),
        (0x13, 0xa1, 0x0e),
        (0xc1, 0x9c, 0x00),
        (0x00, 0x37, 0xda),
        (0x88, 0x17, 0x98),
        (0x3a, 0x96, 0xdd),
        (0xcc, 0xcc, 0xcc),
        (0x76, 0x76, 0x76),
        (0xe7, 0x48, 0x56),
        (0x16, 0xc6, 0x0c),
        (0xf9, 0xf1, 0xa5),
        (0x3b, 0x78, 0xff),
        (0xb4, 0x00, 0x9e),
        (0x61, 0xd6, 0xd6),
        (0xf2, 0xf2, 0xf2),
    ];
    if let Some((r, g, b)) = ANSI16.get(usize::from(index)).copied() {
        return TerminalColor::Rgb { r, g, b };
    }
    if (16..=231).contains(&index) {
        let n = index - 16;
        let r = n / 36;
        let g = (n / 6) % 6;
        let b = n % 6;
        fn channel(value: u16) -> u8 {
            if value == 0 {
                0
            } else {
                u8::try_from(55 + value * 40).unwrap_or(255)
            }
        }
        return TerminalColor::Rgb {
            r: channel(r),
            g: channel(g),
            b: channel(b),
        };
    }
    if (232..=255).contains(&index) {
        let gray = u8::try_from(8 + (index - 232) * 10).unwrap_or(238);
        return TerminalColor::Rgb {
            r: gray,
            g: gray,
            b: gray,
        };
    }
    TerminalColor::Indexed(index)
}

pub(crate) fn terminal_color_from_vt_color(color: vt100::Color) -> TerminalColor {
    match color {
        vt100::Color::Default => TerminalColor::Default,
        vt100::Color::Idx(index) => TerminalColor::Indexed(u16::from(index)),
        vt100::Color::Rgb(r, g, b) => TerminalColor::Rgb { r, g, b },
    }
}

pub(crate) fn cell_attr_from_vt_cell(cell: &vt100::Cell) -> CellAttr {
    CellAttr {
        fg: terminal_color_from_vt_color(cell.fgcolor()),
        bg: terminal_color_from_vt_color(cell.bgcolor()),
        bold: cell.bold(),
        italic: cell.italic(),
        underline: cell.underline(),
        inverse: cell.inverse(),
        strikethrough: false,
        dim: cell.dim(),
    }
}

pub(crate) fn terminal_cell_from_vt_cell(cell: &vt100::Cell) -> TerminalCell {
    if cell.is_wide_continuation() {
        return TerminalCell {
            text: String::new(),
            width: 0,
        };
    }

    TerminalCell {
        text: if cell.has_contents() {
            cell.contents().to_string()
        } else {
            " ".to_string()
        },
        width: if cell.is_wide() { 2 } else { 1 },
    }
}

pub(crate) fn attr_id_for(
    attrs: &mut Vec<CellAttr>,
    index: &mut HashMap<CellAttr, u32>,
    attr: CellAttr,
) -> u32 {
    if let Some(&id) = index.get(&attr) {
        return id;
    }
    let id = u32::try_from(attrs.len()).expect("attr table index overflow");
    attrs.push(attr.clone());
    index.insert(attr, id);
    id
}
