#![windows_subsystem = "windows"]

use eframe::egui;
use native_tls::TlsConnector;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Instant;
use std::sync::Arc;

#[derive(Clone, PartialEq)]
enum Language { English, Polish }

#[derive(Clone)]
enum BlockType {
    Paragraph,
    Div,
    Heading(u8),
    Link(String),
    Image { src: String, alt: String, raw_image: Option<egui::ColorImage>, texture: Option<egui::TextureHandle> },
    Input { name: String, value: String },
    Button { text: String, action_url: Option<String> },
}

#[derive(Clone, Default)]
struct CssStyle {
    align: Option<egui::Align>,
    bg_color: Option<egui::Color32>,
    text_color: Option<egui::Color32>,
    font_size: Option<f32>,
    padding: Option<f32>,
    border_color: Option<egui::Color32>,
    is_bold: bool,
    is_italic: bool,
}

impl CssStyle {
    fn merge(&mut self, other: &CssStyle) {
        if other.align.is_some() { self.align = other.align; }
        if other.bg_color.is_some() { self.bg_color = other.bg_color; }
        if other.text_color.is_some() { self.text_color = other.text_color; }
        if other.font_size.is_some() { self.font_size = other.font_size; }
        if other.padding.is_some() { self.padding = other.padding; }
        if other.border_color.is_some() { self.border_color = other.border_color; }
        if other.is_bold { self.is_bold = true; }
        if other.is_italic { self.is_italic = true; }
    }
}

#[derive(Clone)]
struct RenderBlock {
    text: String,
    block_type: BlockType,
    style: CssStyle,
}

#[allow(dead_code)]
struct FetchResult {
    url: String,
    title: Option<String>,
    favicon_raw: Option<egui::ColorImage>,
    blocks: Vec<RenderBlock>,
    alerts: Vec<String>,
    new_cookie: Option<String>,
    auto_redirect: Option<String>,
    is_secure: bool,
}

#[allow(dead_code)]
enum DownloadMsg {
    Progress(String, f32),
    Done(String, PathBuf),
    Error(String, String),
}

struct Tab {
    url: String,
    title: String,
    favicon_tex: Option<egui::TextureHandle>,
    content: Vec<RenderBlock>,
    is_loading: bool,
    receiver: Option<Receiver<FetchResult>>,
    loaded_at: Option<Instant>,
    cookie_jar: Option<String>,
    is_secure: bool,
}

// --- GRUZ_ENGINE: CORE ---
fn parse_url(url: &str) -> (String, String, bool) {
    let mut clean_url = url.trim();
    let is_secure = clean_url.starts_with("https://");
    clean_url = clean_url.trim_start_matches("http://").trim_start_matches("https://"); 
    
    match clean_url.find('/') {
        Some(idx) => {
            let (host, path) = clean_url.split_at(idx);
            (host.to_string(), path.to_string(), is_secure)
        }
        None => (clean_url.to_string(), "/".to_string(), is_secure),
    }
}

fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http") {
        href.to_string()
    } else {
        let (host, _, is_sec) = parse_url(base);
        let proto = if is_sec { "https" } else { "http" };
        let clean_href = href.trim_start_matches('/');
        format!("{}://{}/{}", proto, host, clean_href)
    }
}

fn fetch_raw(url: &str, cookie: Option<&String>) -> Option<Vec<u8>> {
    let (host, path, is_secure) = parse_url(url);
    let port = if is_secure { 443 } else { 80 };
    
    let stream = TcpStream::connect(format!("{}:{}", host, port)).ok()?;
    let mut request = format!("GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\nUser-Agent: Gruz_Engine/3.1\r\n", path, host);
    
    if let Some(c) = cookie { request.push_str(&format!("Cookie: {}\r\n", c)); }
    request.push_str("\r\n");

    let mut response = Vec::new();

    if is_secure {
        let connector = TlsConnector::new().ok()?;
        let mut tls_stream = connector.connect(&host, stream).ok()?;
        tls_stream.write_all(request.as_bytes()).ok()?;
        tls_stream.read_to_end(&mut response).ok()?;
    } else {
        let mut tcp_stream = stream;
        tcp_stream.write_all(request.as_bytes()).ok()?;
        tcp_stream.read_to_end(&mut response).ok()?;
    }
    
    let mut body_start = 0;
    for i in 0..response.len().saturating_sub(3) {
        if response[i] == b'\r' && response[i+1] == b'\n' && response[i+2] == b'\r' && response[i+3] == b'\n' {
            body_start = i + 4;
            break;
        }
    }
    Some(response[body_start..].to_vec())
}

fn start_download(url: String, tx: Sender<DownloadMsg>) {
    thread::spawn(move || {
        let _ = tx.send(DownloadMsg::Progress(url.clone(), 0.1));
        if let Some(bytes) = fetch_raw(&url, None) {
            if let Ok(user_profile) = env::var("USERPROFILE") {
                let file_name = url.split('/').last().unwrap_or("downloaded_file.dat");
                let mut path = PathBuf::from(user_profile);
                path.push("Downloads");
                path.push(file_name);

                if let Ok(mut file) = File::create(&path) {
                    let _ = file.write_all(&bytes);
                    let _ = tx.send(DownloadMsg::Done(url, path));
                } else {
                    let _ = tx.send(DownloadMsg::Error(url, "Write permission denied / Brak uprawnień zapisu".into()));
                }
            }
        } else {
            let _ = tx.send(DownloadMsg::Error(url, "Network error / Błąd sieci".into()));
        }
    });
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let search = format!("{}=", attr);
    if let Some(idx) = tag.to_lowercase().find(&search) {
        let part = &tag[idx + search.len()..];
        let quote = part.chars().next().unwrap_or('"');
        if quote == '"' || quote == '\'' {
            return Some(part.chars().skip(1).take_while(|&c| c != quote).collect());
        }
    }
    None
}

fn parse_color(c: &str) -> Option<egui::Color32> {
    match c.trim().to_lowercase().as_str() {
        "red" => Some(egui::Color32::RED),
        "blue" => Some(egui::Color32::BLUE),
        "green" => Some(egui::Color32::GREEN),
        "yellow" => Some(egui::Color32::YELLOW),
        "gray" | "grey" => Some(egui::Color32::GRAY),
        "black" => Some(egui::Color32::BLACK),
        "white" => Some(egui::Color32::WHITE),
        _ => None,
    }
}

fn parse_css_style(style_str: &str) -> CssStyle {
    let mut style = CssStyle::default();
    for part in style_str.split(';') {
        let kv: Vec<&str> = part.split(':').map(|s| s.trim()).collect();
        if kv.len() == 2 {
            match kv[0].to_lowercase().as_str() {
                "text-align" => style.align = Some(if kv[1] == "center" { egui::Align::Center } else if kv[1] == "right" { egui::Align::Max } else { egui::Align::Min }),
                "background-color" => style.bg_color = parse_color(kv[1]),
                "color" => style.text_color = parse_color(kv[1]),
                "font-size" => { if let Ok(size) = kv[1].replace("px", "").replace("pt", "").trim().parse::<f32>() { style.font_size = Some(size); } },
                "padding" => { if let Ok(p) = kv[1].replace("px", "").trim().parse::<f32>() { style.padding = Some(p); } },
                "border" | "border-color" => style.border_color = parse_color(kv[1]),
                "font-weight" => if kv[1] == "bold" { style.is_bold = true; },
                "font-style" => if kv[1] == "italic" { style.is_italic = true; },
                _ => {}
            }
        }
    }
    style
}

fn parse_css_sheet(css: &str, global_styles: &mut HashMap<String, CssStyle>) {
    let mut clean_css = String::new();
    let mut in_comment = false;
    let mut chars = css.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            in_comment = true; chars.next();
        } else if c == '*' && chars.peek() == Some(&'/') {
            in_comment = false; chars.next();
        } else if !in_comment {
            clean_css.push(c);
        }
    }

    for rule in clean_css.split('}') {
        if let Some((selectors_str, declarations)) = rule.split_once('{') {
            let parsed_style = parse_css_style(declarations);
            for selector in selectors_str.split(',') {
                let sel = selector.trim().to_lowercase();
                if !sel.is_empty() {
                    let entry = global_styles.entry(sel).or_default();
                    entry.merge(&parsed_style); 
                }
            }
        }
    }
}

fn render_html_to_blocks(raw_html: &str, url: &str, global_styles: &HashMap<String, CssStyle>) -> FetchResult {
    let mut blocks = Vec::new();
    let parsed_alerts = Vec::new();
    let mut parsed_title = None;
    let mut new_cookie = None;
    let mut auto_redirect = None;
    
    let mut current_text = String::new();
    let mut current_style = CssStyle::default();
    let mut current_b_type = BlockType::Paragraph;
    let mut form_action = None;

    let mut chars = raw_html.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '<' {
            if !current_text.trim().is_empty() {
                blocks.push(RenderBlock { text: current_text.clone(), block_type: current_b_type.clone(), style: current_style.clone() });
                current_text.clear();
            }

            let mut tag_content = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '>' { chars.next(); break; }
                tag_content.push(chars.next().unwrap());
            }

            let tag_name = tag_content.to_lowercase().split_whitespace().next().unwrap_or("").to_string();

            if tag_name == "title" {
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

            if tag_name == "script" {
                let mut script_content = String::new();
                while let Some(tc) = chars.next() { script_content.push(tc); if script_content.to_lowercase().ends_with("</script>") { break; } }
                if script_content.to_lowercase().contains("document.cookie") {
                    if let Some(idx) = script_content.to_lowercase().find("document.cookie") {
                        let after_cookie = &script_content[idx..];
                        if let Some(eq_idx) = after_cookie.find('=') {
                            let quote = after_cookie[eq_idx+1..].trim().chars().next().unwrap_or('"');
                            let cookie_val: String = after_cookie[eq_idx+1..].trim().chars().skip(1).take_while(|&ch| ch != quote && ch != ';').collect();
                            if cookie_val.contains("__test=") {
                                new_cookie = Some(cookie_val);
                                if script_content.to_lowercase().contains("location.reload") { auto_redirect = Some(url.to_string()); }
                            }
                        }
                    }
                }
                continue;
            }

            if tag_name == "style" || tag_name == "head" {
                let end_tag = format!("</{}>", tag_name);
                let mut content = String::new();
                while let Some(tc) = chars.next() { content.push(tc); if content.to_lowercase().ends_with(&end_tag) { break; } }
                continue;
            }

            if tag_name.starts_with('/') {
                current_style = CssStyle::default();
                current_b_type = BlockType::Paragraph;
                if tag_name == "/form" { form_action = None; }
                continue;
            }

            let mut cascaded_style = CssStyle::default();

            if let Some(tag_style) = global_styles.get(&tag_name) {
                cascaded_style.merge(tag_style);
            }

            if let Some(class_attr) = extract_attr(&tag_content, "class") {
                for class_name in class_attr.split_whitespace() {
                    let selector = format!(".{}", class_name.to_lowercase());
                    if let Some(class_style) = global_styles.get(&selector) {
                        cascaded_style.merge(class_style);
                    }
                }
            }

            if let Some(id_attr) = extract_attr(&tag_content, "id") {
                let selector = format!("#{}", id_attr.to_lowercase());
                if let Some(id_style) = global_styles.get(&selector) {
                    cascaded_style.merge(id_style);
                }
            }

            if let Some(inline_style) = extract_attr(&tag_content, "style") {
                cascaded_style.merge(&parse_css_style(&inline_style));
            }

            current_style = cascaded_style;

            current_b_type = match tag_name.as_str() {
                "div" => BlockType::Div,
                "h1" => BlockType::Heading(1), "h2" => BlockType::Heading(2),
                "a" => BlockType::Link(extract_attr(&tag_content, "href").unwrap_or_default()),
                "form" => { form_action = extract_attr(&tag_content, "action"); BlockType::Div },
                "input" => {
                    let name = extract_attr(&tag_content, "name").unwrap_or_else(|| "input".into());
                    blocks.push(RenderBlock { text: String::new(), block_type: BlockType::Input { name, value: String::new() }, style: current_style.clone() });
                    BlockType::Paragraph
                },
                "button" => {
                    let text = extract_attr(&tag_content, "value").unwrap_or_else(|| "Submit".into());
                    blocks.push(RenderBlock { text: text.clone(), block_type: BlockType::Button { text, action_url: form_action.clone() }, style: current_style.clone() });
                    BlockType::Paragraph
                },
                "img" => {
                    let src = extract_attr(&tag_content, "src").unwrap_or_default();
                    let alt = extract_attr(&tag_content, "alt").unwrap_or_else(|| "IMG".into());
                    blocks.push(RenderBlock { text: String::new(), block_type: BlockType::Image { src, alt, raw_image: None, texture: None }, style: current_style.clone() });
                    BlockType::Paragraph
                },
                "b" | "strong" => { current_style.is_bold = true; BlockType::Paragraph },
                "i" | "em" => { current_style.is_italic = true; BlockType::Paragraph },
                _ => BlockType::Paragraph,
            };

        } else { current_text.push(c); }
    }

    if !current_text.trim().is_empty() { blocks.push(RenderBlock { text: current_text, block_type: current_b_type, style: current_style }); }
    let is_sec = parse_url(url).2;
    FetchResult { url: url.to_string(), title: parsed_title, favicon_raw: None, blocks, alerts: parsed_alerts, new_cookie, auto_redirect, is_secure: is_sec }
}

fn fetch_http(url: String, cookie: Option<String>) -> FetchResult {
    let body_bytes = match fetch_raw(&url, cookie.as_ref()) {
        Some(b) => b,
        None => return FetchResult { url: url.clone(), title: Some("Error!".into()), favicon_raw: None, blocks: vec![], alerts: vec![], new_cookie: None, auto_redirect: None, is_secure: parse_url(&url).2 },
    };

    let html_string = String::from_utf8_lossy(&body_bytes).to_string();
    let mut global_styles = HashMap::new();

    let mut search_html = html_string.as_str();
    while let Some(start) = search_html.find("<style>") {
        if let Some(end) = search_html[start..].find("</style>") {
            parse_css_sheet(&search_html[start+7..start+end], &mut global_styles);
            search_html = &search_html[start+end+8..];
        } else { break; }
    }

    let mut search_link = html_string.as_str();
    while let Some(start) = search_link.find("<link ") {
        if let Some(end) = search_link[start..].find('>') {
            let tag = &search_link[start..start+end+1];
            if tag.to_lowercase().contains("stylesheet") {
                if let Some(href) = extract_attr(tag, "href") {
                    let css_url = resolve_url(&url, &href);
                    if let Some(css_bytes) = fetch_raw(&css_url, cookie.as_ref()) {
                        parse_css_sheet(&String::from_utf8_lossy(&css_bytes), &mut global_styles);
                    }
                }
            }
            search_link = &search_link[start+end+1..];
        } else { break; }
    }

    let mut result = render_html_to_blocks(&html_string, &url, &global_styles);
    let base_host = parse_url(&url).0;

    for block in &mut result.blocks {
        if let BlockType::Image { src, raw_image, .. } = &mut block.block_type {
            let img_url = resolve_url(&url, src);
            if let Some(img_bytes) = fetch_raw(&img_url, cookie.as_ref()) {
                if let Ok(img) = image::load_from_memory(&img_bytes) { *raw_image = Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as _, img.height() as _], img.to_rgba8().as_flat_samples().as_slice())); }
            }
        }
    }

    let mut fav_path = None;
    let mut search_fav = html_string.as_str();
    while let Some(start) = search_fav.find("<link ") {
        if let Some(end) = search_fav[start..].find('>') {
            let tag = &search_fav[start..start+end+1];
            if tag.to_lowercase().contains("icon") { fav_path = extract_attr(tag, "href"); }
            search_fav = &search_fav[start+end+1..];
        } else { break; }
    }
    
    let fav_url = if let Some(path) = fav_path { resolve_url(&url, &path) } else { format!("http://{}/favicon.ico", base_host) };
    if let Some(img_bytes) = fetch_raw(&fav_url, cookie.as_ref()) {
        if let Ok(img) = image::load_from_memory(&img_bytes) {
            result.favicon_raw = Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as _, img.height() as _], img.to_rgba8().as_flat_samples().as_slice()));
        }
    }

    result
}

struct GruzNetApp {
    tabs: Vec<Tab>,
    active_tab: usize,
    lang: Language,
    is_settings_open: bool,
    active_alerts: Vec<String>,
    dl_receiver: Receiver<DownloadMsg>,
    dl_sender: Sender<DownloadMsg>,
    tex_secure: Option<egui::TextureHandle>,
    tex_notsecure: Option<egui::TextureHandle>,
}

impl Default for GruzNetApp {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tabs: vec![Tab { url: "Gruz://start".to_owned(), title: "Start Page".to_string(), favicon_tex: None, content: vec![RenderBlock { text: "Hi!!\n Type an URL!".to_owned(), block_type: BlockType::Heading(1), style: CssStyle { align: Some(egui::Align::Center), ..Default::default() } }], is_loading: false, receiver: None, loaded_at: Some(Instant::now()), cookie_jar: None, is_secure: true }],
            active_tab: 0,
            lang: Language::English,
            is_settings_open: false,
            active_alerts: Vec::new(),
            dl_receiver: rx,
            dl_sender: tx,
            tex_secure: None,
            tex_notsecure: None,
        }
    }
}

impl eframe::App for GruzNetApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Init secure icons
        if self.tex_secure.is_none() {
            if let Ok(bytes) = std::fs::read("secure.ico") {
                if let Ok(img) = image::load_from_memory(&bytes) { self.tex_secure = Some(ctx.load_texture("sec", egui::ColorImage::from_rgba_unmultiplied([img.width() as _, img.height() as _], img.to_rgba8().as_flat_samples().as_slice()), egui::TextureOptions::LINEAR)); }
            }
            if let Ok(bytes) = std::fs::read("notsecure.ico") {
                if let Ok(img) = image::load_from_memory(&bytes) { self.tex_notsecure = Some(ctx.load_texture("nsec", egui::ColorImage::from_rgba_unmultiplied([img.width() as _, img.height() as _], img.to_rgba8().as_flat_samples().as_slice()), egui::TextureOptions::LINEAR)); }
            }
        }

        // Handle settings window
        let mut settings_open = self.is_settings_open;
        if settings_open {
            let settings_title = match self.lang { Language::English => "Settings", Language::Polish => "Ustawienia" };
            egui::Window::new(settings_title)
                .open(&mut settings_open)
                .collapsible(false)
                .show(ctx, |ui| {
                    let lang_label = match self.lang { Language::English => "App Language", Language::Polish => "Język Aplikacji" };
                    ui.horizontal(|ui| {
                        ui.label(format!("{}:", lang_label));
                        ui.radio_value(&mut self.lang, Language::English, "English");
                        ui.radio_value(&mut self.lang, Language::Polish, "Polski");
                    });
                });
        }
        self.is_settings_open = settings_open;

        // Handle downloads
        if let Ok(msg) = self.dl_receiver.try_recv() {
            match msg {
                DownloadMsg::Done(url, path) => {
                    self.active_alerts.push(match self.lang {
                        Language::English => format!("Downloaded: {} to {:?}", url, path),
                        Language::Polish => format!("Pobrano: {} do {:?}", url, path),
                    })
                },
                DownloadMsg::Error(url, err) => {
                    self.active_alerts.push(match self.lang {
                        Language::English => format!("Error {}: {}", url, err),
                        Language::Polish => format!("Błąd {}: {}", url, err),
                    })
                },
                _ => {}
            }
        }

        let mut repaints_needed = false;
        let mut link_clicked = None;
        let mut download_clicked = None;
        let mut form_submit = None;

        for (_, tab) in self.tabs.iter_mut().enumerate() {
            if tab.is_loading {
                repaints_needed = true;
                if let Some(rx) = &tab.receiver {
                    if let Ok(mut result) = rx.try_recv() {
                        tab.content = result.blocks;
                        tab.title = result.title.unwrap_or_else(|| result.url.clone());
                        tab.loaded_at = Some(Instant::now());
                        tab.is_secure = result.is_secure;
                        if let Some(fav_raw) = result.favicon_raw.take() {
                            tab.favicon_tex = Some(ctx.load_texture("favicon", fav_raw, egui::TextureOptions::LINEAR));
                        }
                        if let Some(new_c) = result.new_cookie { tab.cookie_jar = Some(new_c); }
                        tab.is_loading = false;
                        tab.receiver = None;
                    }
                }
            }
        }

        if repaints_needed { ctx.request_repaint(); }

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut next_active = self.active_tab;
            let mut tab_to_close = None;

            ui.horizontal(|ui| {
                for (idx, tab) in self.tabs.iter().enumerate() {
                    let title = if tab.title.len() > 15 { format!("{}...", &tab.title[..12]) } else { tab.title.clone() };
                    ui.horizontal(|ui| {
                        if tab.is_loading { ui.spinner(); } else if let Some(tex) = &tab.favicon_tex { ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(16.0, 16.0))); } else { ui.label("🌐"); }
                        if ui.selectable_label(self.active_tab == idx, &title).clicked() { next_active = idx; }
                        if self.tabs.len() > 1 { if ui.small_button("x").clicked() { tab_to_close = Some(idx); } }
                    });
                    ui.label(" ");
                }
                
                if ui.button("+").clicked() {
                    let new_tab_title = match self.lang { Language::English => "New Tab", Language::Polish => "Nowa Karta" };
                    self.tabs.push(Tab { url: "Gruz://start".into(), title: new_tab_title.into(), favicon_tex: None, content: vec![], is_loading: false, receiver: None, loaded_at: Some(Instant::now()), cookie_jar: None, is_secure: true });
                    next_active = self.tabs.len() - 1;
                }

                // Push settings button to the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tooltip = match self.lang { Language::English => "Settings", Language::Polish => "Ustawienia" };
                    if ui.button("⚙").on_hover_text(tooltip).clicked() {
                        self.is_settings_open = true;
                    }
                });
            });

            self.active_tab = next_active;
            if let Some(close_idx) = tab_to_close { self.tabs.remove(close_idx); if self.active_tab >= self.tabs.len() { self.active_tab = self.tabs.len() - 1; } }

            ui.separator();

            ui.horizontal(|ui| {
                // Connection security icons with translated hover text
                if self.tabs[self.active_tab].is_secure {
                    if let Some(tex) = &self.tex_secure { 
                        let secure_tooltip = match self.lang { Language::English => "Secure Connection (HTTPS)", Language::Polish => "Bezpieczne połączenie (HTTPS)" };
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(16.0, 16.0)))
                          .on_hover_text(secure_tooltip); 
                    }
                } else {
                    if let Some(tex) = &self.tex_notsecure { 
                        let notsecure_tooltip = match self.lang { Language::English => "Insecure Connection (HTTP)", Language::Polish => "Niezabezpieczone połączenie (HTTP)" };
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(16.0, 16.0)))
                          .on_hover_text(notsecure_tooltip); 
                    }
                }

                let address_lbl = match self.lang { Language::English => "URL:", Language::Polish => "Adres WWW:" };
                let go_btn = match self.lang { Language::English => "Go!", Language::Polish => "Idź!" };

                ui.label(address_lbl);
                let current_url = &mut self.tabs[self.active_tab].url;
                let url_input = ui.add(egui::TextEdit::singleline(current_url).desired_width(500.0));
                
                if ui.button(go_btn).clicked() || (url_input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    link_clicked = Some(self.tabs[self.active_tab].url.clone());
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.tabs[self.active_tab].is_loading {
                    ui.vertical_centered(|ui| { ui.add_space(20.0); ui.spinner(); });
                } else {
                    let base_url = self.tabs[self.active_tab].url.clone();
                    for block in &mut self.tabs[self.active_tab].content {
                        let align = block.style.align.unwrap_or(egui::Align::Min);
                        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 0.0), egui::Layout::top_down(align), |ui| {
                            
                            let mut rt = egui::RichText::new(&block.text);
                            if let Some(c) = block.style.text_color { rt = rt.color(c); }
                            if let Some(size) = block.style.font_size { rt = rt.size(size); }
                            if block.style.is_bold { rt = rt.strong(); }
                            if block.style.is_italic { rt = rt.italics(); }

                            match &mut block.block_type {
                                BlockType::Heading(level) => {
                                    rt = rt.size(36.0 - (*level as f32 * 4.0)).strong();
                                    render_with_style(ui, &block.style, |ui| ui.label(rt));
                                },
                                BlockType::Link(href) => { 
                                    render_with_style(ui, &block.style, |ui| {
                                        let response = ui.link(rt);
                                        if response.clicked() { 
                                            link_clicked = Some(resolve_url(&base_url, href)); 
                                        }
                                        response
                                    });
                                },
                                BlockType::Div => { render_with_style(ui, &block.style, |ui| ui.label(rt)); },
                                BlockType::Input { name: _, value } => {
                                    ui.text_edit_singleline(value);
                                },
                                BlockType::Button { text, action_url } => {
                                    if ui.button(text.as_str()).clicked() { form_submit = action_url.clone(); }
                                }
                                BlockType::Image { alt, raw_image, texture, .. } => {
                                    render_with_style(ui, &block.style, |ui| {
                                        if texture.is_none() && raw_image.is_some() { 
                                            *texture = Some(ui.ctx().load_texture(alt.clone(), raw_image.take().unwrap(), egui::TextureOptions::LINEAR)); 
                                        }
                                        if let Some(tex) = texture { 
                                            ui.add(egui::Image::new(&*tex)) 
                                        } else { 
                                            ui.label("🖼") 
                                        }
                                    });
                                },
                                _ => { render_with_style(ui, &block.style, |ui| ui.label(rt)); }
                            }
                        });
                    }
                }
            });

            // Action Processing Block
            if let Some(url_to_fetch) = link_clicked {
                if url_to_fetch.ends_with(".zip") || url_to_fetch.ends_with(".exe") || url_to_fetch.ends_with(".png") || url_to_fetch.ends_with(".rar") {
                    download_clicked = Some(url_to_fetch.clone());
                } else {
                    self.tabs[self.active_tab].url = url_to_fetch.clone();
                    let (tx, rx) = channel();
                    self.tabs[self.active_tab].is_loading = true;
                    self.tabs[self.active_tab].favicon_tex = None;
                    self.tabs[self.active_tab].receiver = Some(rx);
                    let cookie_to_send = self.tabs[self.active_tab].cookie_jar.clone();
                    thread::spawn(move || { let _ = tx.send(fetch_http(url_to_fetch, cookie_to_send)); });
                }
            }

            if let Some(dl_url) = download_clicked {
                let dl_start_msg = match self.lang { Language::English => "Starting file download...", Language::Polish => "Rozpoczęto pobieranie pliku..." };
                self.active_alerts.push(dl_start_msg.into());
                start_download(dl_url, self.dl_sender.clone());
            }

            if let Some(mut action) = form_submit {
                let mut query = String::new();
                for block in &self.tabs[self.active_tab].content {
                    if let BlockType::Input { name, value } = &block.block_type { query.push_str(&format!("{}={}&", name, value)); }
                }
                let base_url = self.tabs[self.active_tab].url.clone();
                action = resolve_url(&base_url, &action);
                let final_url = format!("{}?{}", action, query.trim_end_matches('&'));
                
                let (tx, rx) = channel();
                self.tabs[self.active_tab].url = final_url.clone();
                self.tabs[self.active_tab].is_loading = true;
                self.tabs[self.active_tab].favicon_tex = None;
                self.tabs[self.active_tab].receiver = Some(rx);
                let cookie_to_send = self.tabs[self.active_tab].cookie_jar.clone();
                thread::spawn(move || { let _ = tx.send(fetch_http(final_url, cookie_to_send)); });
            }
        });
        
        let mut alerts_to_remove = Vec::new();
        for (i, alert_msg) in self.active_alerts.iter().enumerate() {
            let mut close_alert = false;
            let alert_title = match self.lang { Language::English => "Notification", Language::Polish => "Powiadomienie" };
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(format!("js_alert_{}", i)),
                egui::ViewportBuilder::default().with_title(alert_title).with_inner_size([350.0, 120.0]).with_always_on_top(),
                |ctx_vp, _| {
                    egui::CentralPanel::default().show(ctx_vp, |ui| {
                        ui.vertical_centered(|ui| { 
                            ui.add_space(10.0); 
                            ui.label(egui::RichText::new(alert_msg).size(16.0).strong()); 
                            ui.add_space(20.0); 
                            if ui.button("OK").clicked() { close_alert = true; } 
                        });
                    });
                    if ctx_vp.input(|i| i.viewport().close_requested()) { close_alert = true; }
                }
            );
            if close_alert { alerts_to_remove.push(i); }
        }
        for i in alerts_to_remove.into_iter().rev() { self.active_alerts.remove(i); }
    }
}

fn render_with_style<F>(ui: &mut egui::Ui, style: &CssStyle, add_contents: F) -> egui::Response 
where 
    F: FnOnce(&mut egui::Ui) -> egui::Response 
{
    let padding = style.padding.unwrap_or(4.0);
    let align = style.align.unwrap_or(egui::Align::Min);
    
    let mut frame = egui::Frame::none().inner_margin(egui::Margin::same(padding));
    if let Some(bg) = style.bg_color { frame = frame.fill(bg); }
    if let Some(bc) = style.border_color { frame = frame.stroke(egui::Stroke::new(1.0, bc)); }

    frame.show(ui, |ui| {
        ui.with_layout(egui::Layout::top_down(align).with_cross_justify(true), |ui| add_contents(ui)).inner
    }).inner
}

fn main() -> Result<(), eframe::Error> {
    let icon_data = if let Ok(bytes) = std::fs::read("icon.ico") {
        if let Ok(img) = image::load_from_memory(&bytes) { 
            let rgba = img.into_rgba8(); 
            let (w, h) = rgba.dimensions(); 
            Some(Arc::new(egui::IconData { rgba: rgba.into_raw(), width: w, height: h })) 
        } else { None }
    } else { None };

    let mut options = eframe::NativeOptions::default();
    options.viewport.icon = icon_data;
    eframe::run_native("GruzNET Explorer v3.1 Dev Beta", options, Box::new(|_cc| Box::new(GruzNetApp::default())))
}