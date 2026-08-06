//! Terminal banner selection and rendering.
//!
//! Presentation is a frontend concern. Capability facts are passed into the
//! pure selector so this module never inspects process-global terminal state.

use std::io::{self, Write};

const PORTRAIT_WIDTH: usize = 79;
const WORDMARK_WIDTH: usize = 27;
const WORDMARK_INDENT: usize = (PORTRAIT_WIDTH - WORDMARK_WIDTH) / 2;

const PORTRAIT: &[&str] = &[
    "",
    "",
    "                                  :-===-*%%%%*:",
    "                         :*#**%%%%%%%%%%%%%%%%%%%%",
    "                       %%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%=",
    "                     %%%-*%%%%%%%%%%%%%%%%%%%%%%%%%%=%%%*",
    "                    @%. :- %%%%**%%%%%%%%%%%%%%%%%%%%++%%-",
    "                   :* :%%%-:%%%% %%%%+%%%%%%%%%#%%%%%%.%%%%",
    "                     *%%%%%:%%%% *%%%*=%%%%%%%%% %%%%%=*%%%%+",
    "                    =%%%%%%=#+%%  *%%% :%%%%%%%%-:%%%%+ %%%*%:",
    "                    %%%%%%%%: %%    %%%#  %%%%%%: %%%%%  %%:%:",
    "                   %%%#**#%%%  %%   ==%%%* +%%%%: *%%%%*  %%%:",
    "                   %* .*##+ *%% %%=   %%%%. %#%%%   %%%%%  %@:",
    "                   =%%%+#%%%%%%%#*%.   =%*+  #-%%#   +%%%% *+%",
    "                   *%#    %%%%%%%+%: #  %%      %%%   %.%%   ##",
    "                  +%%%% +*%%%%%%%## :%: %:  :%%%: %   %-.%   =#",
    "                 %%%%%%%%%%%%%%%-% +%% *.  ** %+%.-   %  -   -:",
    "              .%%%%%%%%%%%%%%%%%#= %%=  +%%% =%%%.    -     +.",
    "       .:::--: %*#%%%%%%%%%%%%%%%% =%*  %%%   %%%  :   .#    .  ------::.",
    "   :--::.       .*#%%%%%%%%%%%**%+*.%%  %%%%%%%-   -    ==  .......        -==-",
    "                               ---                                .:::..",
];

const WORDMARK: &[&str] = &[
    "▄▄▄   ▄▄▄",
    "████▄████",
    " ▀█████▀   ▀▀█▄ ████▄  ▀▀█▄",
    "▄███████▄ ▄█▀██ ██ ██ ▄█▀██",
    "███▀ ▀███ ▀█▄██ ██ ██ ▀█▄██",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb {
    red: u8,
    green: u8,
    blue: u8,
}

const GRADIENT_START: Rgb = Rgb {
    red: 0xff,
    green: 0x63,
    blue: 0x7d,
};
const GRADIENT_END: Rgb = Rgb {
    red: 0x65,
    green: 0xd4,
    blue: 0xde,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerMode {
    Hidden,
    Plain,
    TrueColor,
}

pub(crate) fn choose_banner_mode(
    selected_surface: bool,
    input_is_terminal: bool,
    output_is_terminal: bool,
    suppressed: bool,
    no_color: bool,
    dumb_terminal: bool,
) -> BannerMode {
    if !selected_surface || !input_is_terminal || !output_is_terminal || suppressed || dumb_terminal
    {
        BannerMode::Hidden
    } else if no_color {
        BannerMode::Plain
    } else {
        BannerMode::TrueColor
    }
}

/// Return the exact endpoint colors at columns 0 and width - 1.
fn interpolate_color(start: Rgb, end: Rgb, column: usize, width: usize) -> Rgb {
    if width <= 1 {
        return start;
    }

    let last = (width - 1) as u32;
    let column = column.min(width - 1) as u32;
    let blend = |start: u8, end: u8| {
        ((u32::from(start) * (last - column) + u32::from(end) * column) / last) as u8
    };

    Rgb {
        red: blend(start.red, end.red),
        green: blend(start.green, end.green),
        blue: blend(start.blue, end.blue),
    }
}

/// Render only the selected presentation mode to the injected writer.
pub(crate) fn write_banner<W: Write>(output: &mut W, mode: BannerMode) -> io::Result<()> {
    match mode {
        BannerMode::Hidden => return Ok(()),
        BannerMode::Plain => {
            write_plain_asset(output, PORTRAIT, 0)?;
            writeln!(output)?;
            write_plain_asset(output, WORDMARK, WORDMARK_INDENT)?;
        }
        BannerMode::TrueColor => {
            write_color_asset(output, PORTRAIT, 0, PORTRAIT_WIDTH)?;
            writeln!(output)?;
            write_color_asset(output, WORDMARK, WORDMARK_INDENT, WORDMARK_WIDTH)?;
        }
    }

    writeln!(output)
}

fn write_plain_asset<W: Write>(output: &mut W, lines: &[&str], indent: usize) -> io::Result<()> {
    for line in lines {
        if !line.is_empty() {
            write!(output, "{:indent$}", "")?;
        }
        writeln!(output, "{line}")?;
    }
    Ok(())
}

fn write_color_asset<W: Write>(
    output: &mut W,
    lines: &[&str],
    indent: usize,
    width: usize,
) -> io::Result<()> {
    for line in lines {
        if line.is_empty() {
            writeln!(output)?;
            continue;
        }

        write!(output, "{:indent$}", "")?;
        for (column, character) in line.chars().enumerate() {
            let color = interpolate_color(GRADIENT_START, GRADIENT_END, column, width);
            write!(
                output,
                "\x1b[38;2;{};{};{}m{character}",
                color.red, color.green, color.blue
            )?;
        }
        writeln!(output, "\x1b[0m")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_includes_exact_endpoints() {
        assert_eq!(
            interpolate_color(GRADIENT_START, GRADIENT_END, 0, PORTRAIT_WIDTH),
            GRADIENT_START
        );
        assert_eq!(
            interpolate_color(
                GRADIENT_START,
                GRADIENT_END,
                PORTRAIT_WIDTH - 1,
                PORTRAIT_WIDTH,
            ),
            GRADIENT_END
        );
    }

    #[test]
    fn gradient_channels_stay_between_the_endpoints() {
        for column in 0..PORTRAIT_WIDTH {
            let color = interpolate_color(GRADIENT_START, GRADIENT_END, column, PORTRAIT_WIDTH);

            assert!((GRADIENT_END.red..=GRADIENT_START.red).contains(&color.red));
            assert!((GRADIENT_START.green..=GRADIENT_END.green).contains(&color.green));
            assert!((GRADIENT_START.blue..=GRADIENT_END.blue).contains(&color.blue));
        }
    }

    #[test]
    fn wordmark_is_centered_in_the_portrait_canvas() {
        let widest = WORDMARK
            .iter()
            .map(|line| line.chars().count())
            .max()
            .expect("wordmark lines");

        assert_eq!(widest, WORDMARK_WIDTH);
        assert_eq!(WORDMARK_INDENT, 26);
        assert_eq!(WORDMARK_INDENT * 2 + WORDMARK_WIDTH, PORTRAIT_WIDTH);
        assert_eq!(
            PORTRAIT.iter().map(|line| line.len()).max(),
            Some(PORTRAIT_WIDTH)
        );
    }

    #[test]
    fn plain_banner_contains_no_escape_sequences() {
        let mut output = Vec::new();

        write_banner(&mut output, BannerMode::Plain).expect("plain banner");
        let output = String::from_utf8(output).expect("UTF-8 banner");

        assert!(!output.contains('\x1b'));
        assert!(output.starts_with("\n\n"));
        assert!(output.contains("████▄████"));
    }

    #[test]
    fn hidden_banner_writes_nothing() {
        let mut output = Vec::new();

        write_banner(&mut output, BannerMode::Hidden).expect("hidden banner");

        assert!(output.is_empty());
    }

    #[test]
    fn banner_surface_selection_honors_every_escape_hatch() {
        assert_eq!(
            choose_banner_mode(true, true, true, false, false, false),
            BannerMode::TrueColor
        );
        assert_eq!(
            choose_banner_mode(true, true, true, false, true, false),
            BannerMode::Plain
        );

        for mode in [
            choose_banner_mode(false, true, true, false, false, false),
            choose_banner_mode(true, false, true, false, false, false),
            choose_banner_mode(true, true, false, false, false, false),
            choose_banner_mode(true, true, true, true, false, false),
            choose_banner_mode(true, true, true, false, false, true),
        ] {
            assert_eq!(mode, BannerMode::Hidden);
        }
    }
}
