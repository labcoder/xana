//! Bounded, presentation-only startup transition shared by terminal surfaces.

use crate::presentation::{PresentationColor, ResolvedPresentation, SemanticToken, WidthClass};
use crossterm::event;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Paragraph},
};
use std::{
    io, thread,
    time::{Duration, Instant},
};
use tachyonfx::{
    CellFilter, Effect, Interpolation, SimpleRng, fx,
    fx::EvolveSymbolSet,
    pattern::{SweepPattern, WavePattern},
    wave::{Oscillator, WaveLayer},
};

const FRAME_TIME: Duration = Duration::from_millis(16);
const MIN_HEIGHT: u16 = 24;

pub(crate) fn play<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    profile: ResolvedPresentation,
) -> io::Result<()> {
    let area = terminal.size()?;
    if profile.reduced_motion
        || !profile.unicode
        || profile.width != WidthClass::Wide
        || area.width < crate::presentation::portrait_width()
        || area.height < MIN_HEIGHT
    {
        return Ok(());
    }

    let mut effect = portrait_effect(profile);
    let mut last_frame = Instant::now();
    loop {
        if event::poll(Duration::ZERO)? {
            let _ = event::read()?;
            break;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_frame);
        last_frame = now;
        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Block::default().style(surface_style(profile)), area);
            let portrait = portrait_area(area);
            let lines = crate::presentation::portrait()
                .iter()
                .map(|line| Line::styled((*line).to_owned(), accent_style(profile)))
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(Text::from(lines)), portrait);
            effect.process(elapsed.into(), frame.buffer_mut(), portrait);
        })?;
        if effect.done() {
            break;
        }
        thread::sleep(FRAME_TIME);
    }
    Ok(())
}

fn portrait_effect(profile: ResolvedPresentation) -> Effect {
    let entry = fx::evolve_into(
        EvolveSymbolSet::BlocksVertical,
        (850, Interpolation::SineOut),
    )
    .with_pattern(SweepPattern::down_to_up(8))
    .with_filter(CellFilter::Text);
    let wave = WavePattern::new(
        WaveLayer::new(Oscillator::sin(0.10, 0.58, 2.4))
            .multiply(Oscillator::cos(0.035, 0.82, 1.3)),
    )
    .with_layer(WaveLayer::new(Oscillator::sin(0.22, 0.16, 3.0)).amplitude(0.55))
    .with_contrast(2)
    .with_transition_width(0.2);
    let surface = profile
        .surface_color(false)
        .map(to_color)
        .unwrap_or(Color::Reset);
    let exit = fx::dissolve_to(
        Style::default().fg(surface).bg(surface),
        (550, Interpolation::SineIn),
    )
    .with_pattern(wave)
    .with_filter(CellFilter::Text)
    .with_rng(SimpleRng::new(0x5841_4e41));
    fx::sequence(&[entry, fx::sleep(100), exit])
}

fn portrait_area(area: Rect) -> Rect {
    let width = crate::presentation::portrait_width().min(area.width);
    let height = u16::try_from(crate::presentation::portrait().len())
        .unwrap_or(u16::MAX)
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn accent_style(profile: ResolvedPresentation) -> Style {
    profile
        .color(SemanticToken::Accent)
        .map(to_color)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}

fn surface_style(profile: ResolvedPresentation) -> Style {
    profile
        .surface_color(false)
        .map(to_color)
        .map_or_else(Style::default, |color| Style::default().bg(color))
}

fn to_color(color: PresentationColor) -> Color {
    match color {
        PresentationColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        PresentationColor::Indexed(index) => Color::Indexed(index),
        PresentationColor::Red => Color::Red,
        PresentationColor::Green => Color::Green,
        PresentationColor::Yellow => Color::Yellow,
        PresentationColor::Magenta => Color::Magenta,
        PresentationColor::Cyan => Color::Cyan,
        PresentationColor::DarkGray => Color::DarkGray,
        PresentationColor::Black => Color::Black,
        PresentationColor::White => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_stays_centered_and_bounded() {
        assert_eq!(
            portrait_area(Rect::new(0, 0, 100, 40)),
            Rect::new(10, 9, 79, 21)
        );
        assert_eq!(
            portrait_area(Rect::new(2, 3, 40, 10)),
            Rect::new(2, 3, 40, 10)
        );
    }

    #[test]
    fn effect_has_the_requested_bounded_lifecycle() {
        let mut effect = portrait_effect(ResolvedPresentation::test_plain());
        let area = Rect::new(0, 0, 79, 21);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        assert!(!effect.done());
        effect.process(tachyonfx::Duration::from_millis(1_499), &mut buffer, area);
        assert!(!effect.done());
        effect.process(tachyonfx::Duration::from_millis(1), &mut buffer, area);
        assert!(effect.done());
    }
}
