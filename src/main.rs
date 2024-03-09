#![feature(let_chains)]
#![feature(slice_as_chunks)]

use bytes::Buf;
use std::{
    cell::{Cell, RefCell},
    io::stdout,
    thread::sleep,
    time::Duration,
};
use style::palette::tailwind;

use anyhow::Result;

use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{prelude::*, widgets::*};
use serialport::{SerialPort, UsbPortInfo};

const PALETTES: [tailwind::Palette; 4] = [
    tailwind::BLUE,
    tailwind::EMERALD,
    tailwind::INDIGO,
    tailwind::RED,
];

const INFO_TEXT: &str =
    "(q) quit | (j) move up | (j) move down | (→) next color | (←) previous color";

enum update {
    Log,
    Status,
    All,
}

#[repr(u8)]
enum Cmd {
    SysGetSernum = 0x83,
    Read = 0x90,
    Write = 0x91,
}

fn gen_cmd(station: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
    let info = [&[station, 1 + data.len() as u8, cmd], data].concat();
    let bcc = info.iter().fold(0u8, |data, old| data ^ old);
    [&[0xAA], info.as_slice(), &[bcc, 0xBB]].concat()
}

fn parse_data<'a>(data: &'a [u8]) -> Result<(u8, &'a [u8])> {
    let bcc_pos = 3 + data[2] as usize;
    let last_pos = bcc_pos + 1;
    let bcc = data[1..bcc_pos].iter().fold(0u8, |data, old| data ^ old);
    if data[0] != 0xAA || data[last_pos] != 0xBB || bcc != data[bcc_pos] {
        return Err(anyhow::anyhow!("Data corrupted"));
    }
    let status = data[3];
    let data = &data[4..bcc_pos];
    Ok((status, data))
}

static mut BUF: [u8; 129] = [0u8; 129];

fn main() -> Result<()> {
    let color = &PALETTES[0];
    let buffer_bg = tailwind::SLATE.c950;
    let header_bg = color.c900;
    let header_fg = tailwind::SLATE.c200;
    let row_fg = tailwind::SLATE.c200;
    let selected_style_fg = color.c400;
    let normal_row_color = tailwind::SLATE.c950;
    let alt_row_color = tailwind::SLATE.c900;
    let footer_border_color = color.c400;

    let mut responses = vec![];
    let mut saved_data = [[0u8; 4]; 6];
    let mut saved_conf = [0u8; 4];
    let mut port: Option<Box<dyn SerialPort>> = None;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut ustate = Some(update::All);
    let mut last_data = None;
    let mut offset = TableState::new();

    loop {
        match ustate {
            Some(update::Log) | Some(update::All) => {
                terminal.draw(|f| {
                    let area = f.size();

                    let layout =
                        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                            .split(area);
                    let widths = [
                        Constraint::Percentage(3),
                        Constraint::Percentage(15),
                        Constraint::Percentage(12),
                        Constraint::Percentage(70),
                    ];

                    let header_style = Style::default().fg(header_fg).bg(header_bg);
                    let selected_style = Style::default()
                        .add_modifier(Modifier::REVERSED)
                        .fg(selected_style_fg);
                    let selected_style = Style::default()
                        .add_modifier(Modifier::REVERSED)
                        .fg(selected_style_fg);

                    let bar = " █ ";
                    let table = Table::new(
                        responses.iter().cloned(),
                        // .skip(offset)
                        // .take(layout[0].height as usize),
                        widths,
                    )
                    .header(
                        Row::new(vec!["Type", "Time", "Status", "Data"])
                            .style(Style::new().bold())
                            .style(header_style),
                    )
                    .highlight_style(selected_style)
                    .highlight_symbol(Text::from(vec![
                        "".into(),
                        bar.into(),
                        bar.into(),
                        "".into(),
                    ]))
                    .bg(buffer_bg);

                    f.render_stateful_widget(table, layout[0], &mut offset);
                    let create_block = |title| {
                        Block::default()
                            .borders(Borders::ALL)
                            .style(Style::default().fg(Color::Gray))
                            .title(Span::styled(
                                title,
                                Style::default().add_modifier(Modifier::BOLD),
                            ))
                    };

                    let mut text = vec![Line::from("Saved data:")];
                    text.extend(saved_data.map(|x| Line::from(hex::encode_upper(x))));
                    let para = Paragraph::new(text)
                        .style(Style::default().fg(Color::Gray))
                        .block(create_block("Default alignment (Left), with wrap"))
                        .wrap(Wrap { trim: true });

                    f.render_widget(para, layout[1]);
                })?;

                ustate.take();
            }
            Some(update::Status) | Some(update::All) => {
                // terminal.draw(|f| {
                //     let area = f.size();

                //     let layout =
                //         Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                //             .split(area);
                // })?;
            }
            None => {}
        }

        let push = |typ: &str, status: &str, data: &str| {
            responses.push(Row::new(vec![
                typ.to_string(),
                Local::now().to_string(),
                status.to_string(),
                data.to_string(),
            ]));
            // let i = offset.selected().unwrap_or(0);
            offset.select(Some(responses.len() - 1));
            // offset.select(Some)
            // offset = responses.len() - 1;
            ustate = Some(update::Log);
        };

        let push = RefCell::new(push);

        let mut exe = |port: &mut Option<Box<dyn SerialPort>>,
                       cmd: Cmd,
                       send_buf: &[u8]|
         -> Option<(u8, &'static [u8])> {
            let wrap = || -> Result<(u8, &'static [u8])> {
                if let Some(ref mut port) = port {
                    port.write(gen_cmd(0, cmd as u8, send_buf).as_slice())?;
                    unsafe { port.read(&mut BUF)? };
                    let d = unsafe { parse_data(&BUF) };
                    if let Ok((_, d)) = d {
                        last_data = Some(d);
                    }
                    d
                } else {
                    Err(anyhow::anyhow!("Port not active"))
                }
            };
            match wrap() {
                Ok(o) => {
                    push.borrow_mut()(
                        "R",
                        o.0.to_string().as_str(),
                        hex::encode_upper(o.1).as_str(),
                    );
                    Some(o)
                }
                Err(e) => {
                    push.borrow_mut()("E", "Error", e.to_string().as_str());
                    None
                }
            }
        };

        if event::poll(Duration::from_millis(50)).is_ok() {
            if let event::Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('r') => {
                        let mut count = 0;
                        loop {
                            if count > 2 {
                                // saved_data[i as usize - 1].copy_from_slice(&[0u8; 4]);
                                break;
                            }
                            if let Some((_, d)) = exe(&mut port, Cmd::Read, &[0, 1])
                                && d.len() == 4 * 6
                            {
                                let (d, _) = d.as_chunks::<4>();
                                for (i, x) in d.iter().enumerate() {
                                    saved_data[i].copy_from_slice(x);
                                }
                                break;
                            }
                            sleep(Duration::from_millis(50));
                            count += 1;
                        }
                    }
                    // KeyCode::Char('s') => {
                    //     saved_data = last_data;
                    //     if let Some(d) = last_data {
                    //         push("I", "Info", format!("Saved hexes : {:02X?}", d).as_str());
                    //     }
                    // }
                    KeyCode::Char('j') => {
                        if let Some(i) = offset.selected() {
                            offset.select(Some((i + 1).min(responses.len() - 1)));
                            ustate = Some(update::Log);
                        }
                    }
                    KeyCode::Char('k') => {
                        if let Some(i) = offset.selected() {
                            offset.select(Some(i.saturating_sub(1)));
                            ustate = Some(update::Log);
                        }
                    }
                    KeyCode::Char('G') => {
                        offset.select(Some(responses.len() - 1));
                        ustate = Some(update::Log);
                    }
                    KeyCode::Char('g') => {
                        offset.select(Some(0));
                        ustate = Some(update::Log);
                    }
                    KeyCode::Char('C') => {
                        if let Some((_, d)) = exe(&mut port, Cmd::Read, &[0, 0])
                            && d.len() == 4
                        {
                            saved_conf.copy_from_slice(&d[0..4]);
                            push.borrow_mut()(
                                "I",
                                "Info",
                                format!("Saved conf : {:02X?}", saved_conf).as_str(),
                            );
                        }
                        ustate = Some(update::Log);
                    }
                    KeyCode::Char('c') => {
                        let mut find_port = || -> Result<()> {
                            let ports = serialport::available_ports()?;
                            for p in ports {
                                let mut p = Some(
                                    serialport::new(&p.port_name, 9600)
                                        .timeout(Duration::from_millis(100))
                                        .open()?,
                                );
                                if let Some((status, data)) = exe(&mut p, Cmd::SysGetSernum, &[])
                                    && status == 0x00
                                    && data.len() == 8
                                {
                                    port.take();
                                    port = p;
                                    return Ok(());
                                }
                            }
                            Err(anyhow::anyhow!("Unable to find a suitable port"))
                        };

                        match find_port() {
                            Ok(_) => push.borrow_mut()("I", "Info", "Connected"),
                            Err(e) => push.borrow_mut()("E", "Error", e.to_string().as_str()),
                        }
                    }
                    KeyCode::Char('D') => {
                        for (i, x) in saved_data.iter().enumerate() {
                            let mut count = 0;
                            loop {
                                if count > 5 {
                                    break;
                                }
                                if exe(
                                    &mut port,
                                    Cmd::Write,
                                    [&[0x00u8, i as u8 + 1u8], x as &[_]].concat().as_slice(),
                                )
                                .is_some_and(|(s, _)| s == 0x00)
                                {
                                    push.borrow_mut()(
                                        "I",
                                        "Info",
                                        format!("Wrote {:02X?}", x).as_str(),
                                    );
                                    break;
                                }
                                sleep(Duration::from_millis(50));
                                count += 1;
                            }
                            sleep(Duration::from_millis(50));
                        }
                    }
                    KeyCode::Char('q') => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
