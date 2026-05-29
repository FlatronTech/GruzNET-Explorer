#![windows_subsystem = "windows"]

use eframe::egui;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

// Representation of a formatted text block
#[derive(Clone)]
struct RenderBlock {
    text: String,
    align: egui::Align,
    bg_color: Option<egui::Color32>,
}

// Result of threaded fetch operation
struct FetchResult {
    url: String,
    title: Option<String>,
    blocks: Vec<RenderBlock>,
}

// Representation of a single tab
struct Tab {
    url: String,
    title: String,
    content: Vec<RenderBlock>,
    is_loading: bool,
    receiver: Option<Receiver<FetchResult>>,
}

fn parse_url(url: &str) -> (String, String) {
    let mut clean_url = url.trim();
    clean_url = clean_url.trim_start_matches("http://");
    clean_url = clean_url.trim_start_matches("https://"); 

    match clean_url.find('/') {
        Some(idx) => {
            let (host, path) = clean_url.split_at(idx);
            (host.to_string(), path.to_string())
        }
        None => (clean_url.to_string(), "/".to_string()),
    }
}

fn parse_color(color_str: &str) -> Option<egui::Color32> {
    let clean = color_str.trim().to_lowercase();
    if clean.starts_with('#') {
        let hex = clean.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
            return Some(egui::Color32::from_rgb(r, g, b));
        }
    } else {
        match clean.as_str() {
            "red" => return Some(egui::Color32::from_rgb(255, 0, 0)),
            "blue" => return Some(egui::Color32::from_rgb(0, 0, 255)),
            "green" => return Some(egui::Color32::from_rgb(0, 255, 0)),
            "yellow" => return Some(egui::Color32::from_rgb(255, 255, 0)),
            "gray" | "grey" => return Some(egui::Color32::from_rgb(128, 128, 128)),
            "black" => return Some(egui::Color32::from_rgb(0, 0, 0)),
            "white" => return Some(egui::Color32::from_rgb(255, 255, 255)),
            _ => {}
        }
    }
    None
}

fn parse_css_style(style_str: &str) -> (egui::Align, Option<egui::Color32>) {
    let mut align = egui::Align::Min;
    let mut bg_color = None;

    for part in style_str.split(';') {
        let kv: Vec<&str> = part.split(':').map(|s| s.trim()).collect();
        if kv.len() == 2 {
            match kv[0] {
                "text-align" => match kv[1] {
                    "center" => align = egui::Align::Center,
                    "right" => align = egui::Align::Max,
                    _ => align = egui::Align::Min,
                },
                "background-color" => {
                    bg_color = parse_color(kv[1]);
                }
                _ => {}
            }
        }
    }
    (align, bg_color)
}

// Updated renderer extracting title and handling Gruz-JS
fn render_html_to_blocks(raw_response: &str) -> (Option<String>, Vec<RenderBlock>) {
    let html_body = match raw_response.find("\r\n\r\n") {
        Some(idx) => &raw_response[idx + 4..],
        None => raw_response,
    };

    let mut blocks = Vec::new();
    let mut current_text = String::new();
    let mut parsed_title = None;
    
    let mut current_align = egui::Align::Min;
    let mut current_bg = None;

    let mut chars = html_body.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '<' {
            if !current_text.trim().is_empty() {
                blocks.push(RenderBlock {
                    text: current_text.clone(),
                    align: current_align,
                    bg_color: current_bg,
                });
                current_text.clear();
            }

            let mut tag_content = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '>' {
                    chars.next(); 
                    break;
                }
                tag_content.push(chars.next().unwrap());
            }

            let tag_lower = tag_content.to_lowercase();

            // GETTING THE TITLE
            if tag_lower == "title" {
                let mut title_text = String::new();
                while let Some(tc) = chars.next() {
                    title_text.push(tc);
                    if title_text.to_lowercase().ends_with("</title>") {
                        parsed_title = Some(title_text[..title_text.len()-8].trim().to_string());
                        break;
                    }
                }
                continue;
            }
            
            // GRUZ-JS ENGINE: Getting the content of <script>!
            if tag_lower.starts_with("script") {
                let mut script_content = String::new();
                while let Some(sc) = chars.next() {
                    script_content.push(sc);
                    if script_content.to_lowercase().ends_with("</script>") { break; }
                }
                
                // Proste "wykonywanie" skryptu: szukamy document.write("coś")
                if let Some(start_idx) = script_content.find("document.write(") {
                    let js_args = &script_content[start_idx + 15..];
                    let quote_type = js_args.chars().next().unwrap_or('"');
                    if quote_type == '"' || quote_type == '\'' {
                        let extracted_js_text: String = js_args.chars().skip(1).take_while(|&ch| ch != quote_type).collect();
                        blocks.push(RenderBlock {
                            text: format!("[Gruz-JS] {}", extracted_js_text),
                            align: egui::Align::Min,
                            bg_color: Some(egui::Color32::from_rgb(40, 40, 40)),
                        });
                    }
                }
                continue;
            }

            if tag_lower.starts_with("style") {
                let mut style_end = String::new();
                while let Some(sc) = chars.next() {
                    style_end.push(sc);
                    if style_end.to_lowercase().contains("</style>") { break; }
                }
                continue;
            }

            if tag_lower.starts_with('/') {
                current_align = egui::Align::Min;
                current_bg = None;
            } else if let Some(style_idx) = tag_lower.find("style=") {
                let style_part = &tag_content[style_idx + 6..];
                let quote_char = style_part.chars().next().unwrap_or('"');
                if quote_char == '"' || quote_char == '\'' {
                    let actual_style: String = style_part.chars().skip(1).take_while(|&ch| ch != quote_char).collect();
                    let (align, bg) = parse_css_style(&actual_style);
                    current_align = align;
                    current_bg = bg;
                }
            }
        } else {
            current_text.push(c);
        }
    }

    if !current_text.trim().is_empty() {
        blocks.push(RenderBlock {
            text: current_text,
            align: current_align,
            bg_color: current_bg,
        });
    }

    for block in &mut blocks {
        let cleaned: Vec<&str> = block.text.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        block.text = cleaned.join("\n");
    }
    blocks.retain(|b| !b.text.is_empty());
    
    if blocks.is_empty() {
        blocks.push(RenderBlock {
            text: "No content to display on this page...".to_string(),
            align: egui::Align::Min,
            bg_color: None,
        });
    }

    (parsed_title, blocks)
}

// Function called on a separate thread!
fn fetch_http(url: String) -> FetchResult {
    let (host, path) = parse_url(&url);
    
    let mut stream = match TcpStream::connect(format!("{}:80", host)) {
        Ok(s) => s,
        Err(e) => return FetchResult {
            url,
            title: Some("Error".to_string()),
            blocks: vec![RenderBlock { text: format!("Error connecting to {}: {}", host, e), align: egui::Align::Min, bg_color: None }]
        },
    };

    let request = format!("GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: GruzNET-GUI/0.6\r\n\r\n", path, host);

    if stream.write_all(request.as_bytes()).is_err() {
        return FetchResult { url, title: Some("Error".to_string()), blocks: vec![RenderBlock { text: "Oh noes! There was an error sending the request".to_string(), align: egui::Align::Min, bg_color: None }] };
    }

    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return FetchResult { url, title: Some("Error".to_string()), blocks: vec![RenderBlock { text: "Oh noes! There was an error reading the response".to_string(), align: egui::Align::Min, bg_color: None }] };
    }

    let (title, blocks) = render_html_to_blocks(&response);
    FetchResult { url, title, blocks }
}

struct GruzNetApp {
    tabs: Vec<Tab>,
    active_tab: usize,
    show_version: bool,
}

impl Default for GruzNetApp {
    fn default() -> Self {
        Self {
            tabs: vec![Tab {
                url: "Gruz://start".to_owned(),
                title: "Start Page".to_string(),
                content: vec![RenderBlock {
                    text: "Welcome!! This is a new experimental browser!\nType an address and click 'Go!'" .to_owned(),
                    align: egui::Align::Min,
                    bg_color: None,
                }],
                is_loading: false,
                receiver: None,
            }],
            active_tab: 0,
            show_version: false,
        }
    }
}

impl eframe::App for GruzNetApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // THREAD CHECKING
        let mut repaints_needed = false;
        for tab in &mut self.tabs {
            if tab.is_loading {
                repaints_needed = true; 
                if let Some(rx) = &tab.receiver {
                    if let Ok(result) = rx.try_recv() {
                        tab.content = result.blocks;
                        // If <title> was found, use it; otherwise fallback to URL as title
                        tab.title = result.title.unwrap_or_else(|| result.url.clone());
                        tab.is_loading = false;
                        tab.receiver = None;
                    }
                }
            }
        }
        
        // Force repaint if any tab is still loading, to keep the spinner animating
        if repaints_needed {
            ctx.request_repaint();
        }

        if self.show_version {
            egui::Window::new("About").collapsible(false).resizable(false).open(&mut self.show_version).show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("GruzNET Explorer");
                    ui.label("Version: v0.6");
                    ui.separator();
                    ui.label("Copyright Flatron Tech 2026");
                });
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut next_active = self.active_tab;
            let mut tab_to_close = None;
            let mut create_tab = false;

            // --- TABS SECTION ---
            ui.horizontal(|ui| {
                for (idx, tab) in self.tabs.iter().enumerate() {
                    let mut display_title = tab.title.clone();
                    if display_title.len() > 15 {
                        display_title = format!("{}...", &display_title[..12]);
                    }

                    ui.horizontal(|ui| {
                        // A spinner next to the tab title if it's loading!
                        if tab.is_loading {
                            ui.spinner();
                        }
                        
                        if ui.selectable_label(self.active_tab == idx, &display_title).clicked() {
                            next_active = idx;
                        }
                        if self.tabs.len() > 1 {
                            if ui.small_button("x").clicked() {
                                tab_to_close = Some(idx);
                            }
                        }
                    });
                    ui.label(" ");
                }

                if ui.button("+").clicked() {
                    create_tab = true;
                }
            });

            self.active_tab = next_active;

            if create_tab {
                self.tabs.push(Tab {
                    url: "Gruz://start".to_owned(),
                    title: "Nowa karta".to_string(),
                    content: vec![RenderBlock { text: "Wpisz adres...".to_owned(), align: egui::Align::Min, bg_color: None }],
                    is_loading: false,
                    receiver: None,
                });
                self.active_tab = self.tabs.len() - 1;
            }

            if let Some(close_idx) = tab_to_close {
                self.tabs.remove(close_idx);
                if self.active_tab >= self.tabs.len() {
                    self.active_tab = self.tabs.len() - 1;
                }
            }

            ui.separator();

            // --- ADDRESS BAR ---
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Version").clicked() { self.show_version = true; }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label("URL:");
                        let current_url = &mut self.tabs[self.active_tab].url;
                        let url_input = ui.add(egui::TextEdit::singleline(current_url).desired_width(400.0));
                        
                        // Zaczynamy pobieranie w osobnym WĄTKU
                        if ui.button("Go!").clicked() || (url_input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                            let url_to_fetch = self.tabs[self.active_tab].url.clone();
                            if url_to_fetch == "Gruz://start" {
                                self.tabs[self.active_tab].title = "Start Page".to_string();
                                self.tabs[self.active_tab].content = vec![RenderBlock { text: "Welcome!! This is a new experimental browser!".to_owned(), align: egui::Align::Min, bg_color: None }];
                            } else {
                                // Odpalamy wątek!
                                let (tx, rx) = channel();
                                self.tabs[self.active_tab].is_loading = true;
                                self.tabs[self.active_tab].receiver = Some(rx);
                                self.tabs[self.active_tab].title = "Loading...".to_string();
                                self.tabs[self.active_tab].content.clear(); // Clearing the content while loading

                                thread::spawn(move || {
                                    let result = fetch_http(url_to_fetch);
                                    let _ = tx.send(result);
                                });
                            }
                        }
                    });
                });
            });

            ui.separator();

            // --- PAGE DISPLAY ENGINE ---
            egui::ScrollArea::vertical().show(ui, |ui| {
                let current_content = self.tabs[self.active_tab].content.clone();
                let is_loading = self.tabs[self.active_tab].is_loading;

                if is_loading {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        ui.spinner(); // Large spinner in the center of the page!
                        ui.label("Fetching Gruz from the server...");
                    });
                } else {
                    for block in current_content {
                        let desired_size = egui::vec2(ui.available_width(), 0.0);
                        ui.allocate_ui_with_layout(desired_size, egui::Layout::top_down(block.align), |ui| {
                            if let Some(bg) = block.bg_color {
                                egui::Frame::none()
                                    .fill(bg)
                                    .inner_margin(egui::Margin::same(6.0))
                                    .show(ui, |ui| {
                                        ui.with_layout(egui::Layout::top_down(block.align).with_cross_justify(true), |ui| {
                                            ui.add(egui::Label::new(&block.text).wrap(true));
                                        });
                                    });
                            } else {
                                ui.add(egui::Label::new(&block.text).wrap(true));
                            }
                        });
                        ui.add_space(4.0);
                    }
                }
            });
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("GruzNET Explorer"),
        ..Default::default()
    };
    
    eframe::run_native(
        "GruzNET Explorer",
        options,
        Box::new(|_cc| Box::new(GruzNetApp::default())),
    )
}