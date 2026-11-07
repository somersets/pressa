use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use pressa_core::{
    Effort, OutputFormat, RAW_EXTENSIONS, Report, collect_inputs, convert_batch, is_supported_path,
};

use crate::settings::{ResizeMode, Settings};

const SETTINGS_KEY: &str = "settings";
const GOOD: Color32 = Color32::from_rgb(90, 190, 110);
const WARN: Color32 = Color32::from_rgb(230, 160, 60);
const BAD: Color32 = Color32::from_rgb(230, 90, 90);

enum Status {
    Queued,
    Done(Report),
    Failed(String),
}

struct Row {
    path: PathBuf,
    size: u64,
    status: Status,
}

struct Job {
    rx: Receiver<(usize, Result<Report, String>)>,
    cancel: Arc<AtomicBool>,
    /// Batch index -> row index.
    rows: Vec<usize>,
    done: usize,
    started: Instant,
}

pub struct App {
    settings: Settings,
    rows: Vec<Row>,
    known: HashSet<PathBuf>,
    job: Option<Job>,
    last_run: Option<Duration>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        let mut app =
            Self { settings, rows: Vec::new(), known: HashSet::new(), job: None, last_run: None };
        // Files passed on the command line, e.g. dropped onto the executable.
        app.add_paths(std::env::args_os().skip(1).map(PathBuf::from).collect());
        app
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        for path in collect_inputs(&paths, self.settings.recursive) {
            if !is_supported_path(&path) || !self.known.insert(path.clone()) {
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.rows.push(Row { path, size, status: Status::Queued });
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        // Re-run everything if nothing is left to do, otherwise only what hasn't succeeded.
        let pending: Vec<usize> =
            (0..self.rows.len()).filter(|&i| !matches!(self.rows[i].status, Status::Done(_))).collect();
        let rows = if pending.is_empty() { (0..self.rows.len()).collect() } else { pending };
        if rows.is_empty() {
            return;
        }
        for &i in &rows {
            self.rows[i].status = Status::Queued;
        }

        let paths: Vec<PathBuf> = rows.iter().map(|&i| self.rows[i].path.clone()).collect();
        let opts = self.settings.to_options();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let cancel_flag = cancel.clone();
        std::thread::spawn(move || {
            convert_batch(&paths, &opts, &cancel_flag, |i, result| {
                let _ = tx.send((i, result.map_err(|e| format!("{e:#}"))));
                ctx.request_repaint();
            });
            ctx.request_repaint();
        });
        self.job = Some(Job { rx, cancel, rows, done: 0, started: Instant::now() });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        loop {
            match job.rx.try_recv() {
                Ok((i, result)) => {
                    job.done += 1;
                    self.rows[job.rows[i]].status = match result {
                        Ok(report) => Status::Done(report),
                        Err(e) => Status::Failed(e),
                    };
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.last_run = Some(job.started.elapsed());
                    self.job = None;
                    break;
                }
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.running(), |ui| {
                if ui.button("Add files…").clicked() {
                    let mut exts: Vec<&str> = vec![
                        "jpg", "jpeg", "png", "webp", "avif", "gif", "bmp", "tif", "tiff", "ico",
                        "tga", "qoi", "jxl", "svg", "exr", "hdr", "dds", "pnm", "ppm", "pgm",
                        "heic", "heif", "hif",
                    ];
                    exts.extend_from_slice(RAW_EXTENSIONS);
                    if let Some(files) =
                        rfd::FileDialog::new().add_filter("Images", &exts).pick_files()
                    {
                        self.add_paths(files);
                    }
                }
                if ui.button("Add folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.add_paths(vec![dir]);
                    }
                if ui.button("Clear list").clicked() {
                    self.rows.clear();
                    self.known.clear();
                    self.last_run = None;
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (input, output, count) = self.totals();
                if count > 0 {
                    let change = (output as f64 / input.max(1) as f64 - 1.0) * 100.0;
                    ui.label(
                        RichText::new(format!("{:+.1}%", change))
                            .strong()
                            .color(if change <= 0.0 { GOOD } else { WARN }),
                    );
                    ui.label(format!("{count} done: {} -> {}", human(input), human(output)));
                } else if !self.rows.is_empty() {
                    let total: u64 = self.rows.iter().map(|r| r.size).sum();
                    ui.label(format!("{} files, {}", self.rows.len(), human(total)));
                }
            });
        });
    }

    fn totals(&self) -> (u64, u64, usize) {
        self.rows.iter().fold((0, 0, 0), |(i, o, n), r| match &r.status {
            Status::Done(rep) => (i + rep.input_size, o + rep.output_size, n + 1),
            _ => (i, o, n),
        })
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        ui.add_space(6.0);
        ui.heading("Output");
        ui.add_space(4.0);

        let format = s.output_format();
        egui::Grid::new("output").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Format");
            let mut selected = format;
            egui::ComboBox::from_id_salt("format")
                .selected_text(selected.map_or("Same as input", |f| f.name()))
                .width(150.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, None, "Same as input");
                    for f in OutputFormat::ALL {
                        ui.selectable_value(&mut selected, Some(f), f.name());
                    }
                });
            if selected != format {
                s.set_output_format(selected);
            }
            ui.end_row();

            let can_lossless = selected.is_none_or(|f| f.supports_lossless());
            let lossless = s.lossless && can_lossless;
            let lossy = selected.is_none_or(|f| f.is_lossy()) && !lossless;

            ui.label("Quality");
            ui.add_enabled(lossy, egui::Slider::new(&mut s.quality, 1..=100));
            ui.end_row();

            ui.label("");
            ui.add_enabled(can_lossless, egui::Checkbox::new(&mut s.lossless, "Lossless"));
            ui.end_row();

            ui.label("Effort");
            let mut effort = s.effort();
            egui::ComboBox::from_id_salt("effort")
                .selected_text(effort.name())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for e in Effort::ALL {
                        ui.selectable_value(&mut effort, e, e.name());
                    }
                });
            s.effort = Effort::ALL.iter().position(|e| *e == effort).unwrap_or(1) as u8;
            ui.end_row();
        });
        if let Some(hint) = format_hint(format, s.lossless) {
            ui.label(RichText::new(hint).small().weak());
        }

        ui.add_space(10.0);
        ui.separator();
        ui.heading("Resize");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.resize_mode, ResizeMode::None, "Original");
            ui.radio_value(&mut s.resize_mode, ResizeMode::Fit, "Fit in box");
            ui.radio_value(&mut s.resize_mode, ResizeMode::Percent, "Percent");
        });
        match s.resize_mode {
            ResizeMode::None => {}
            ResizeMode::Fit => {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.max_width).range(1..=65535).suffix(" px"));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut s.max_height).range(1..=65535).suffix(" px"));
                });
                ui.label(RichText::new("Keeps aspect ratio, never upscales.").small().weak());
            }
            ResizeMode::Percent => {
                ui.add(egui::Slider::new(&mut s.percent, 1.0..=100.0).suffix(" %"));
            }
        }

        ui.add_space(10.0);
        ui.separator();
        ui.heading("File size");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.target_enabled, "Limit to");
            ui.add_enabled(
                s.target_enabled,
                egui::DragValue::new(&mut s.target_kb).range(1..=1_000_000).speed(5).suffix(" KB"),
            );
        });
        ui.label(
            RichText::new("Finds the best quality (up to the slider) that fits.").small().weak(),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.heading("Save to");
        ui.add_space(4.0);
        let mut next_to = s.output_dir.is_none();
        if ui.radio_value(&mut next_to, true, "Next to originals").changed() {
            s.output_dir = None;
        }
        ui.horizontal(|ui| {
            if (ui.radio_value(&mut next_to, false, "Folder:").clicked() || ui.button("Choose…").clicked())
                && let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    s.output_dir = Some(dir);
                }
        });
        if let Some(dir) = &s.output_dir {
            ui.label(RichText::new(dir.display().to_string()).small().monospace())
                .on_hover_text(dir.display().to_string());
        }
        ui.checkbox(&mut s.overwrite, "Overwrite existing files")
            .on_hover_text("Off: adds a -min suffix instead of replacing a file.");
        ui.checkbox(&mut s.recursive, "Include subfolders when adding folders");
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                if ui.add(egui::Button::new("Cancel").min_size([110.0, 32.0].into())).clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                let total = job.rows.len();
                let progress = job.done as f32 / total.max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{} / {}  ·  {:.1}s", job.done, total, job.started.elapsed().as_secs_f32()))
                        .animate(true),
                );
            } else {
                let pending = self.rows.iter().filter(|r| !matches!(r.status, Status::Done(_))).count();
                let label = match (pending, self.rows.len()) {
                    (_, 0) => "Convert".to_string(),
                    (0, n) => format!("Convert {n} again"),
                    (p, _) => format!("Convert {p} file{}", if p == 1 { "" } else { "s" }),
                };
                let button = egui::Button::new(RichText::new(label).strong())
                    .min_size([180.0, 32.0].into());
                if ui.add_enabled(!self.rows.is_empty(), button).clicked() {
                    self.start(ui.ctx());
                }
                if let Some(t) = self.last_run {
                    let failed = self.rows.iter().filter(|r| matches!(r.status, Status::Failed(_))).count();
                    let mut text = format!("Finished in {:.2}s", t.as_secs_f32());
                    if failed > 0 {
                        text += &format!(", {failed} failed");
                    }
                    ui.label(RichText::new(text).color(if failed > 0 { BAD } else { GOOD }));
                }
            }
        });
        ui.add_space(6.0);
    }

    fn file_table(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Drop images or folders here\n\nJPEG · PNG · WebP · AVIF · HEIC · GIF · TIFF · BMP · JPEG XL · SVG · RAW …")
                        .size(18.0)
                        .weak(),
                );
            });
            return;
        }

        let running = self.running();
        let mut remove = None;
        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink([false, false])
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(80.0))
            .column(Column::exact(200.0))
            .column(Column::exact(70.0))
            .column(Column::exact(56.0))
            .header(22.0, |mut header| {
                for title in ["File", "Size", "Result", "Saved", ""] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, self.rows.len(), |mut row| {
                    let i = row.index();
                    let r = &self.rows[i];
                    row.col(|ui| {
                        let name = r.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                        ui.label(name).on_hover_text(r.path.display().to_string());
                    });
                    row.col(|ui| {
                        ui.label(human(r.size));
                    });
                    match &r.status {
                        Status::Queued => {
                            row.col(|ui| {
                                if running {
                                    ui.spinner();
                                } else {
                                    ui.label(RichText::new("Waiting").weak());
                                }
                            });
                            row.col(|_| {});
                        }
                        Status::Done(rep) => {
                            row.col(|ui| {
                                let mut text = format!("{} · {}", rep.output_format.name(), human(rep.output_size));
                                if rep.kept_original {
                                    text += " (kept)";
                                }
                                let resp = ui.label(text);
                                let mut tip = format!(
                                    "{}\n{}×{} · {:.2}s",
                                    rep.output.display(),
                                    rep.width,
                                    rep.height,
                                    rep.elapsed.as_secs_f64()
                                );
                                if let Some(q) = rep.quality_used {
                                    tip += &format!("\nQuality used: {q}");
                                }
                                if rep.kept_original {
                                    tip += "\nRe-encoding didn't make it smaller, so the original was copied.";
                                }
                                if rep.target_missed {
                                    tip += "\nCouldn't reach the target size.";
                                }
                                resp.on_hover_text(tip);
                            });
                            row.col(|ui| {
                                let change = rep.change_percent();
                                let color = if rep.target_missed || change > 0.0 { WARN } else { GOOD };
                                ui.label(RichText::new(format!("{:+.0}%", change)).color(color));
                            });
                        }
                        Status::Failed(e) => {
                            row.col(|ui| {
                                ui.label(RichText::new(e.as_str()).color(BAD)).on_hover_text(e);
                            });
                            row.col(|_| {});
                        }
                    }
                    row.col(|ui| {
                        if let Status::Done(rep) = &r.status {
                            if ui.small_button("Open").on_hover_text("Show in folder").clicked() {
                                reveal(&rep.output);
                            }
                        } else if !running && ui.small_button("×").on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            });
        if let Some(i) = remove {
            let row = self.rows.remove(i);
            self.known.remove(&row.path);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_job();

        let ctx = ui.ctx().clone();
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() && !self.running() {
            self.add_paths(dropped);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(6.0);
            self.toolbar(ui);
            ui.add_space(6.0);
        });
        egui::Panel::right("settings")
            .resizable(false)
            .exact_size(300.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(!self.running(), |ui| self.settings_panel(ui));
                });
            });
        egui::Panel::bottom("actions").show(ui, |ui| self.bottom_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.file_table(ui);
            if hovering {
                let rect = ui.max_rect();
                let painter = ui.painter();
                painter.rect_filled(rect, 8.0, Color32::from_rgba_unmultiplied(80, 140, 255, 30));
                painter.rect_stroke(
                    rect.shrink(2.0),
                    8.0,
                    egui::Stroke::new(2.0, Color32::from_rgb(80, 140, 255)),
                    egui::StrokeKind::Inside,
                );
            }
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}

fn format_hint(format: Option<OutputFormat>, lossless: bool) -> Option<&'static str> {
    Some(match format? {
        OutputFormat::Jpeg => "mozjpeg: smaller JPEGs at the same quality.",
        OutputFormat::Png if lossless => "oxipng: lossless optimization.",
        OutputFormat::Png => "Reduces to 256 colors with dithering (like pngquant); falls back to lossless if quality can't be met.",
        OutputFormat::Webp => "libwebp. Lossless works well for graphics and screenshots.",
        OutputFormat::Avif => "Best compression for photos, but the slowest encoder.",
        OutputFormat::Gif => "256 colors, first frame only.",
        OutputFormat::Ico => "Scaled down to fit 256×256.",
        OutputFormat::Tiff => "Lossless, Deflate-compressed.",
        _ => return None,
    })
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn reveal(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}
