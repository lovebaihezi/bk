use std::collections::HashMap;
use std::fs::File;
use std::io::{stdout, Read, Write};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    queue,
    style::{Attribute, Print},
    terminal::{self, ClearType},
};

use roxmltree::Document;

struct Epub {
    container: zip::ZipArchive<File>,
}

impl Epub {
    fn new(path: &str) -> std::io::Result<Self> {
        let file = File::open(path)?;

        Ok(Epub {
            container: zip::ZipArchive::new(file)?,
        })
    }
    fn render<'a>(acc: &mut Vec<Vec<&'a str>>, n: roxmltree::Node<'a, '_>) {
        if n.is_text() {
            let text = n.text().unwrap();
            if !text.trim().is_empty() {
                let last = acc.last_mut().unwrap();
                last.push(text);
            }
            return;
        }

        match n.tag_name().name() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                acc.push(vec!["\x1b\x5b1m"]);
                for c in n.children() {
                    Self::render(acc, c);
                }
                acc.push(vec!["\x1b\x5b0m"]);
            }
            "blockquote" | "p" => {
                acc.push(Vec::new());
                for c in n.children() {
                    Self::render(acc, c);
                }
                acc.push(Vec::new());
            }
            "li" => {
                acc.push(vec!["- "]);
                for c in n.children() {
                    Self::render(acc, c);
                }
                acc.push(Vec::new());
            }
            "br" => acc.push(Vec::new()),
            _ => {
                for c in n.children() {
                    Self::render(acc, c);
                }
            }
        }
    }
    fn get_text(&mut self, name: &str) -> String {
        let mut text = String::new();
        self.container
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }
    fn get_toc(&mut self) -> Vec<(String, String)> {
        let xml = self.get_text("META-INF/container.xml");
        let doc = Document::parse(&xml).unwrap();
        let path = doc
            .descendants()
            .find(|n| n.has_tag_name("rootfile"))
            .unwrap()
            .attribute("full-path")
            .unwrap();

        // manifest - resource paths
        // spine - chapter list
        // toc - chapter names, may have fewer items than spine
        let mut manifest = HashMap::new();
        let xml = self.get_text(path);
        let doc = Document::parse(&xml).unwrap();
        doc.root_element()
            .children()
            .find(|n| n.has_tag_name("manifest"))
            .unwrap()
            .children()
            .filter(|n| n.is_element())
            .for_each(|n| {
                manifest.insert(
                    n.attribute("id").unwrap(),
                    n.attribute("href").unwrap(),
                );
            });
        let rootdir = std::path::Path::new(&path).parent().unwrap();
        let paths: Vec<&str> = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("spine"))
            .unwrap()
            .children()
            .filter(|n| n.is_element())
            .map(|n| manifest.remove(n.attribute("idref").unwrap()).unwrap())
            .collect();

        let mut toc = HashMap::new();
        if doc.root_element().attribute("version") == Some("3.0") {
            let path = doc
                .root_element()
                .children()
                .find(|n| n.has_tag_name("manifest"))
                .unwrap()
                .children()
                .find(|n| n.attribute("properties") == Some("nav"))
                .unwrap()
                .attribute("href")
                .unwrap();
            let xml = self.get_text(rootdir.join(path).to_str().unwrap());
            let doc = Document::parse(&xml).unwrap();

            doc.descendants()
                .find(|n| n.has_tag_name("nav"))
                .unwrap()
                .descendants()
                .filter(|n| n.has_tag_name("a"))
                .for_each(|n| {
                    let path = n.attribute("href").unwrap().to_string();
                    let text = n
                        .descendants()
                        .filter(|n| n.is_text())
                        .map(|n| n.text().unwrap())
                        .collect();
                    toc.insert(path, text);
                })
        } else {
            let path = manifest.get("ncx").unwrap();
            let xml = self.get_text(rootdir.join(path).to_str().unwrap());
            let doc = Document::parse(&xml).unwrap();

            doc.descendants()
                .find(|n| n.has_tag_name("navMap"))
                .unwrap()
                .descendants()
                .filter(|n| n.has_tag_name("navPoint"))
                .for_each(|n| {
                    let path = n
                        .descendants()
                        .find(|n| n.has_tag_name("content"))
                        .unwrap()
                        .attribute("src")
                        .unwrap()
                        .to_string();
                    let text = n
                        .descendants()
                        .find(|n| n.has_tag_name("text"))
                        .unwrap()
                        .text()
                        .unwrap()
                        .to_string();
                    toc.insert(path, text);
                })
        }

        paths
            .into_iter()
            .enumerate()
            .map(|(i, path)| {
                let title = toc.remove(path).unwrap_or_else(|| i.to_string());
                let path = rootdir.join(path).to_str().unwrap().to_string();
                (title, path)
            })
            .collect()
    }
}

fn wrap(text: String, width: u16) -> Vec<String> {
    // XXX assumes a char is 1 unit wide
    // TODO break at dash/hyphen
    let mut wrapped = Vec::new();

    let mut start = 0;
    let mut space = 0;
    let mut line = 0;
    let mut word = 0;

    for (i, c) in text.char_indices() {
        if c == ' ' {
            space = i;
            word = 0;
        } else {
            word += 1;
        }
        if line == width {
            wrapped.push(String::from(&text[start..space]));
            start = space + 1;
            line = word;
        } else {
            line += 1;
        }
    }
    wrapped.push(String::from(&text[start..]));
    wrapped
}

struct Position(String, usize, usize);

enum Direction {
    Forward,
    Backward,
}

enum Mode {
    Help,
    Nav,
    Read,
    Search,
}

struct Bk {
    mode: Mode,
    epub: Epub,
    cols: u16,
    chapter: Vec<String>,
    chapter_idx: usize,
    nav_idx: usize,
    nav_top: usize,
    pos: usize,
    rows: usize,
    toc: Vec<(String, String)>,
    pad: u16,
    search: String,
}

impl Bk {
    fn new(mut epub: Epub, pos: &Position, pad: u16) -> Self {
        let (cols, rows) = terminal::size().unwrap();
        let mut bk = Bk {
            mode: Mode::Read,
            chapter: Vec::new(),
            chapter_idx: 0,
            nav_idx: 0,
            nav_top: 0,
            toc: epub.get_toc(),
            epub,
            pos: pos.2,
            pad,
            cols,
            rows: rows as usize,
            search: String::new(),
        };
        bk.get_chapter(pos.1);
        bk
    }
    fn run(&mut self) -> crossterm::Result<()> {
        let mut stdout = stdout();
        queue!(
            stdout,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            //event::EnableMouseCapture
        )?;
        terminal::enable_raw_mode()?;

        loop {
            match self.mode {
                Mode::Read => self.render_read(),
                Mode::Help => self.render_help(),
                Mode::Nav => self.render_nav(),
                Mode::Search => self.render_search(),
            }
            match event::read()? {
                Event::Key(e) => match self.mode {
                    Mode::Read => {
                        if self.match_read(e.code) {
                            break;
                        }
                    }
                    Mode::Help => self.mode = Mode::Read,
                    Mode::Nav => self.match_nav(e.code),
                    Mode::Search => self.match_search(e.code),
                },
                Event::Resize(cols, rows) => {
                    self.cols = cols;
                    self.rows = rows as usize;
                    self.get_chapter(self.chapter_idx);
                }
                // TODO
                Event::Mouse(_) => (),
            }
        }

        queue!(
            stdout,
            terminal::LeaveAlternateScreen,
            cursor::Show,
            //event::DisableMouseCapture
        )?;
        //stdout.flush()?;
        terminal::disable_raw_mode()
    }
    fn get_chapter(&mut self, idx: usize) {
        let xml = self.epub.get_text(&self.toc[idx].1);
        let doc = Document::parse(&xml).unwrap();
        let body = doc.root_element().last_element_child().unwrap();
        let mut chapter = Vec::new();
        Epub::render(&mut chapter, body);

        let width = self.cols - (self.pad * 2);
        self.chapter = Vec::with_capacity(chapter.len() * 2);
        for line in chapter {
            self.chapter.append(&mut wrap(line.concat(), width));
        }
        self.chapter_idx = idx;
    }
    fn scroll_down(&mut self, n: usize) {
        if self.rows < self.chapter.len() - self.pos {
            self.pos += n;
        } else if self.chapter_idx < self.toc.len() - 1 {
            self.get_chapter(self.chapter_idx + 1);
            self.pos = 0;
        }
    }
    fn scroll_up(&mut self, n: usize) {
        if self.pos > 0 {
            self.pos = self.pos.saturating_sub(n);
        } else if self.chapter_idx > 0 {
            self.get_chapter(self.chapter_idx - 1);
            self.pos = (self.chapter.len() / self.rows) * self.rows;
        }
    }
    fn really_search(&mut self, dir: Direction) {
        match dir {
            Direction::Forward => {
                if let Some(i) = self.chapter[self.pos + 1..]
                    .iter()
                    .position(|s| s.contains(&self.search))
                {
                    self.pos += i + 1;
                }
            }
            Direction::Backward => {
                if let Some(i) = self.chapter[..self.pos]
                    .iter()
                    .rposition(|s| s.contains(&self.search))
                {
                    self.pos = i;
                }
            }
        }
    }
    fn match_search(&mut self, kc: KeyCode) {
        match kc {
            KeyCode::Esc => {
                self.search = String::new();
                self.mode = Mode::Read;
            }
            KeyCode::Enter => {
                self.mode = Mode::Read;
                self.really_search(Direction::Forward);
            }
            KeyCode::Char(c) => {
                self.search.push(c);
            }
            _ => (),
        }
    }
    fn match_nav(&mut self, kc: KeyCode) {
        match kc {
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => {
                self.mode = Mode::Read
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Char('l') => {
                self.get_chapter(self.nav_idx);
                self.pos = 0;
                self.mode = Mode::Read;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.nav_idx < self.toc.len() - 1 {
                    self.nav_idx += 1;
                    if self.nav_idx == self.nav_top + self.rows {
                        self.nav_top += 1;
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.nav_idx > 0 {
                    if self.nav_idx == self.nav_top {
                        self.nav_top -= 1;
                    }
                    self.nav_idx -= 1;
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.nav_idx = 0;
                self.nav_top = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.nav_idx = self.toc.len() - 1;
                self.nav_top = self.toc.len().saturating_sub(self.rows);
            }
            _ => (),
        }
    }
    fn match_read(&mut self, kc: KeyCode) -> bool {
        match kc {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Tab => self.start_nav(),
            KeyCode::F(1) | KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/') => {
                self.search = String::new();
                self.mode = Mode::Search;
            }
            KeyCode::Char('N') => {
                self.really_search(Direction::Backward);
            }
            KeyCode::Char('n') => {
                self.really_search(Direction::Forward);
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.pos = (self.chapter.len() / self.rows) * self.rows;
            }
            KeyCode::Home | KeyCode::Char('g') => self.pos = 0,
            KeyCode::Char('d') => {
                self.scroll_down(self.rows / 2);
            }
            KeyCode::Char('u') => {
                self.scroll_up(self.rows / 2);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_up(1);
            }
            KeyCode::Left
            | KeyCode::PageUp
            | KeyCode::Char('b')
            | KeyCode::Char('h') => {
                self.scroll_up(self.rows);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_down(1);
            }
            KeyCode::Right
            | KeyCode::PageDown
            | KeyCode::Char('f')
            | KeyCode::Char('l')
            | KeyCode::Char(' ') => {
                self.scroll_down(self.rows);
            }
            _ => (),
        }
        false
    }
    fn render_search(&self) {
        let mut stdout = stdout();
        queue!(
            stdout,
            cursor::MoveTo(0, self.rows as u16),
            terminal::Clear(ClearType::CurrentLine),
            Print(&self.search)
        )
        .unwrap();
        stdout.flush().unwrap();
    }
    fn start_nav(&mut self) {
        self.nav_idx = self.chapter_idx;
        self.nav_top = self.nav_idx.saturating_sub(self.rows - 1);
        self.mode = Mode::Nav;
    }
    fn render_nav(&self) {
        let mut stdout = stdout();
        queue!(stdout, terminal::Clear(ClearType::All)).unwrap();

        let end = std::cmp::min(self.nav_top + self.rows, self.toc.len());
        for (i, line) in self.toc[self.nav_top..end].iter().enumerate() {
            let s = if self.nav_idx == self.nav_top + i {
                format!(
                    "{}{}{}",
                    Attribute::Reverse,
                    line.0,
                    Attribute::Reset
                )
            } else {
                line.0.to_string()
            };
            queue!(stdout, cursor::MoveTo(0, i as u16), Print(s)).unwrap();
        }
        stdout.flush().unwrap();
    }
    fn render_help(&self) {
        // TODO const?
        let text = r#"
                   Esc q  Quit
                    F1 ?  Help
                       /  Search
                     Tab  Table of Contents

PageDown Right Space f l  Page Down
         PageUp Left b h  Page Up
                       d  Half Page Down
                       u  Half Page Up
                  Down j  Line Down
                    Up k  Line Up
                  Home g  Chapter Start
                   End G  Chapter End
                       n  Search Forward
                       N  Search Backward
                   "#;

        let mut stdout = stdout();
        queue!(stdout, terminal::Clear(ClearType::All)).unwrap();
        for (i, line) in text.lines().enumerate() {
            queue!(stdout, cursor::MoveTo(0, i as u16), Print(line)).unwrap();
        }
        stdout.flush().unwrap();
    }
    fn render_read(&self) {
        let mut stdout = stdout();
        queue!(stdout, terminal::Clear(ClearType::All)).unwrap();

        let end = std::cmp::min(self.pos + self.rows, self.chapter.len());
        for (y, line) in self.chapter[self.pos..end].iter().enumerate() {
            queue!(stdout, cursor::MoveTo(self.pad, y as u16), Print(line))
                .unwrap();
        }
        stdout.flush().unwrap();
    }
}

fn restore() -> Option<Position> {
    let path = std::env::args().nth(1);
    let save_path =
        format!("{}/.local/share/bk", std::env::var("HOME").unwrap());
    let save = std::fs::read_to_string(save_path);

    let get_save = |s: String| {
        let mut lines = s.lines();
        Position(
            lines.next().unwrap().to_string(),
            lines.next().unwrap().parse::<usize>().unwrap(),
            lines.next().unwrap().parse::<usize>().unwrap(),
        )
    };

    match (save, path) {
        (Err(_), None) => None,
        (Err(_), Some(path)) => Some(Position(path, 0, 0)),
        (Ok(save), None) => Some(get_save(save)),
        (Ok(save), Some(path)) => {
            let save = get_save(save);
            if save.0 == path {
                Some(save)
            } else {
                Some(Position(path, 0, 0))
            }
        }
    }
}

fn main() {
    let pos = restore().unwrap_or_else(|| {
        println!("usage: bk path");
        std::process::exit(1);
    });

    let epub = Epub::new(&pos.0).unwrap_or_else(|e| {
        println!("error reading epub: {}", e);
        std::process::exit(1);
    });

    let mut bk = Bk::new(epub, &pos, 3);
    // crossterm really shouldn't error
    bk.run().unwrap();

    std::fs::write(
        format!("{}/.local/share/bk", std::env::var("HOME").unwrap()),
        format!("{}\n{}\n{}", pos.0, bk.chapter_idx, bk.pos),
    )
    .unwrap_or_else(|e| {
        println!("error saving position: {}", e);
        std::process::exit(1);
    });
}
