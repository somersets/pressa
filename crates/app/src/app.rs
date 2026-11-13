use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use pressa_core::{
    Effort, ErrorKind, InputFile, MIRROR_SUFFIX, OutputFormat, RAW_EXTENSIONS, Report, VIDEO_EXTENSIONS,
    VideoCodec, VideoOptions, collect_inputs, convert_batch, encoder_label, error_kind, ffmpeg_path,
    is_supported, pick_encoder,
};

use crate::i18n::Lang;
use crate::settings::{ResizeMode, Settings};

const SETTINGS_KEY: &str = "settings";
const GOOD: Color32 = Color32::from_rgb(90, 190, 110);
const WARN: Color32 = Color32::from_rgb(230, 160, 60);
const BAD: Color32 = Color32::from_rgb(230, 90, 90);

enum Status {
    Queued,
    Done(Report),
    Failed { kind: Option<ErrorKind>, detail: String },
}

struct Row {
    input: InputFile,
    size: u64,
    status: Status,
    /// Encoding progress of a video in this run.
    progress: Option<f32>,
}

enum Msg {
    Progress(usize, f32),
    Done(usize, Result<Report, (Option<ErrorKind>, String)>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// Batch index -> row index.
    rows: Vec<usize>,
    done: usize,
    started: Instant,
}

/// Which video encoder each codec will use with GPU on; filled in the background
/// because probing GPU encoders takes a moment. `None` value: FFmpeg missing.
type EncoderMap = Arc<Mutex<HashMap<VideoCodec, Option<&'static str>>>>;

pub struct App {
    settings: Settings,
    rows: Vec<Row>,
    known: HashSet<PathBuf>,
    job: Option<Job>,
    last_run: Option<Duration>,
    gpu_encoders: EncoderMap,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        let gpu_encoders = EncoderMap::default();
        let map = gpu_encoders.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            for codec in VideoCodec::ALL {
                let enc = pick_encoder(&VideoOptions { codec, hardware: true, ..Default::default() });
                map.lock().unwrap().insert(codec, enc);
            }
            ctx.request_repaint();
        });
        let mut app = Self {
            settings,
            rows: Vec::new(),
            known: HashSet::new(),
            job: None,
            last_run: None,
            gpu_encoders,
        };
        // Files passed on the command line, e.g. dropped onto the executable or sent via "Send to".
        app.add_paths(std::env::args_os().skip(1).map(PathBuf::from).collect());
        app
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        for input in collect_inputs(&paths, self.settings.recursive) {
            if !is_supported(&input.path) || !self.known.insert(input.path.clone()) {
                continue;
            }
            let size = std::fs::metadata(&input.path).map(|m| m.len()).unwrap_or(0);
            self.rows.push(Row { input, size, status: Status::Queued, progress: None });
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
            self.rows[i].progress = None;
        }

        let inputs: Vec<InputFile> = rows.iter().map(|&i| self.rows[i].input.clone()).collect();
        let opts = self.settings.to_options();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let cancel_flag = cancel.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            convert_batch(
                &inputs,
                &opts,
                &cancel_flag,
                |i, f| {
                    let _ = progress_tx.send(Msg::Progress(i, f));
                    progress_ctx.request_repaint();
                },
                |i, result| {
                    let result = result.map_err(|e| (error_kind(&e), format!("{e:#}")));
                    let _ = tx.send(Msg::Done(i, result));
                    ctx.request_repaint();
                },
            );
            ctx.request_repaint();
        });
        self.job = Some(Job { rx, cancel, rows, done: 0, started: Instant::now() });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        loop {
            match job.rx.try_recv() {
                Ok(Msg::Progress(i, f)) => self.rows[job.rows[i]].progress = Some(f),
                Ok(Msg::Done(i, result)) => {
                    job.done += 1;
                    let row = &mut self.rows[job.rows[i]];
                    row.progress = None;
                    row.status = match result {
                        Ok(report) => Status::Done(report),
                        Err((kind, detail)) => Status::Failed { kind, detail },
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
        let l = self.settings.lang;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.running(), |ui| {
                if ui.button(l.t("Add files…", "Добавить файлы…")).clicked() {
                    let mut exts: Vec<&str> = vec![
                        "jpg", "jpeg", "png", "webp", "avif", "gif", "bmp", "tif", "tiff", "ico",
                        "tga", "qoi", "jxl", "svg", "exr", "hdr", "dds", "pnm", "ppm", "pgm",
                        "heic", "heif", "hif",
                    ];
                    exts.extend_from_slice(RAW_EXTENSIONS);
                    exts.extend_from_slice(VIDEO_EXTENSIONS);
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter(l.t("Photos and videos", "Фото и видео"), &exts)
                        .pick_files()
                    {
                        self.add_paths(files);
                    }
                }
                if ui.button(l.t("Add folder…", "Добавить папку…")).clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_paths(vec![dir]);
                }
                if ui.button(l.t("Clear list", "Очистить список")).clicked() {
                    self.rows.clear();
                    self.known.clear();
                    self.last_run = None;
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut lang = self.settings.lang;
                egui::ComboBox::from_id_salt("lang")
                    .selected_text(lang.native_name())
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for option in Lang::ALL {
                            ui.selectable_value(&mut lang, option, option.native_name());
                        }
                    });
                self.settings.lang = lang;
                ui.separator();

                let (input, output, count) = self.totals();
                if count > 0 {
                    let change = (output as f64 / input.max(1) as f64 - 1.0) * 100.0;
                    ui.label(
                        RichText::new(format!("{change:+.1}%"))
                            .strong()
                            .color(if change <= 0.0 { GOOD } else { WARN }),
                    );
                    let done = match l {
                        Lang::En => format!("{} done", l.files(count)),
                        Lang::Ru => format!("Готово {}", l.files(count)),
                    };
                    ui.label(format!("{done}: {} -> {}", human(l, input), human(l, output)));
                } else if !self.rows.is_empty() {
                    let total: u64 = self.rows.iter().map(|r| r.size).sum();
                    ui.label(format!("{}, {}", l.files(self.rows.len()), human(l, total)));
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
        let l = self.settings.lang;
        let gpu_encoders = self.gpu_encoders.lock().unwrap().clone();
        let s = &mut self.settings;
        ui.add_space(6.0);
        ui.heading(l.t("Photos", "Фото"));
        ui.add_space(4.0);

        let format = s.output_format();
        egui::Grid::new("output").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(l.t("Format", "Формат"));
            let same = l.t("Same as input", "Как у исходного");
            let mut selected = format;
            egui::ComboBox::from_id_salt("format")
                .selected_text(selected.map_or(same, |f| f.name()))
                .width(150.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, None, same);
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

            ui.label(l.t("Quality", "Качество"));
            ui.add_enabled(lossy, egui::Slider::new(&mut s.quality, 1..=100));
            ui.end_row();

            ui.label("");
            ui.add_enabled(can_lossless, egui::Checkbox::new(&mut s.lossless, l.t("Lossless", "Без потерь")));
            ui.end_row();

            ui.label(l.t("Effort", "Режим"));
            let mut effort = s.effort();
            egui::ComboBox::from_id_salt("effort")
                .selected_text(l.effort(effort))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for e in Effort::ALL {
                        ui.selectable_value(&mut effort, e, l.effort(e));
                    }
                });
            s.effort = Effort::ALL.iter().position(|e| *e == effort).unwrap_or(1) as u8;
            ui.end_row();
        });
        if let Some(hint) = format_hint(l, format, s.lossless) {
            ui.label(RichText::new(hint).small().weak());
        }

        ui.add_space(10.0);
        ui.separator();
        ui.heading(l.t("Video", "Видео"));
        ui.add_space(4.0);
        let codec = s.video_codec();
        egui::Grid::new("video").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(l.t("Codec", "Кодек"));
            let mut selected = codec;
            egui::ComboBox::from_id_salt("codec")
                .selected_text(selected.name())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for c in VideoCodec::ALL {
                        ui.selectable_value(&mut selected, c, c.name());
                    }
                });
            if selected != codec {
                s.set_video_codec(selected);
            }
            ui.end_row();

            ui.label(l.t("Quality", "Качество"));
            ui.add(egui::Slider::new(&mut s.video_quality, 1..=100));
            ui.end_row();

            ui.label("");
            ui.checkbox(&mut s.video_gpu, l.t("Use GPU (faster)", "Ускорение видеокартой"))
                .on_hover_text(l.t(
                    "Several times faster; files are slightly larger at the same quality.",
                    "В несколько раз быстрее; файлы немного больше при том же качестве.",
                ));
            ui.end_row();
        });
        let codec = s.video_codec();
        ui.label(RichText::new(codec_hint(l, codec)).small().weak());
        let encoder = match (ffmpeg_path(), s.video_gpu) {
            (None, _) => None,
            (Some(_), false) => Some(Some("cpu")),
            (Some(_), true) => gpu_encoders.get(&codec).copied(),
        };
        match encoder {
            None if ffmpeg_path().is_none() => {
                ui.label(RichText::new(l.t(
                    "FFmpeg not found: videos can't be compressed.",
                    "FFmpeg не найден: видео сжать не получится.",
                ))
                .small()
                .color(BAD));
            }
            Some(Some(enc)) => {
                let label = if enc == "cpu" { "CPU" } else { encoder_label(enc) };
                ui.label(RichText::new(format!("{}: {label}", l.t("Encoder", "Кодировщик"))).small());
            }
            _ => {
                ui.label(RichText::new(l.t("Checking encoders…", "Проверка кодировщиков…")).small().weak());
            }
        }
        ui.label(
            RichText::new(l.t("Saved as MP4 (MKV stays MKV).", "Сохраняется в MP4 (MKV остаётся MKV)."))
                .small()
                .weak(),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.heading(l.t("Resize", "Размер"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.resize_mode, ResizeMode::None, l.t("Original", "Исходный"));
            ui.radio_value(&mut s.resize_mode, ResizeMode::Fit, l.t("Fit in box", "Вписать"));
            ui.radio_value(&mut s.resize_mode, ResizeMode::Percent, l.t("Percent", "Процент"));
        });
        match s.resize_mode {
            ResizeMode::None => {}
            ResizeMode::Fit => {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.max_width).range(1..=65535).suffix(" px"));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut s.max_height).range(1..=65535).suffix(" px"));
                });
            }
            ResizeMode::Percent => {
                ui.add(egui::Slider::new(&mut s.percent, 1.0..=100.0).suffix(" %"));
            }
        }
        ui.label(
            RichText::new(l.t(
                "Photos and videos. Keeps aspect ratio, never upscales.",
                "Фото и видео. Сохраняет пропорции, не увеличивает.",
            ))
            .small()
            .weak(),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.heading(l.t("File size", "Размер файла"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.target_enabled, l.t("Limit to", "Не больше"));
            ui.add_enabled(
                s.target_enabled,
                egui::DragValue::new(&mut s.target_kb).range(1..=10_000_000).speed(5).suffix(l.t(" KB", " КБ")),
            );
        });
        ui.label(
            RichText::new(l.t(
                "Photos: best quality (up to the slider) that fits. Videos: bitrate that fits.",
                "Фото: лучшее качество (не выше ползунка), которое помещается. Видео: подходящий битрейт.",
            ))
            .small()
            .weak(),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.heading(l.t("Save to", "Сохранять"));
        ui.add_space(4.0);
        let mut next_to = s.output_dir.is_none();
        if ui.radio_value(&mut next_to, true, l.t("Next to originals", "Рядом с оригиналами")).changed() {
            s.output_dir = None;
        }
        ui.horizontal(|ui| {
            if (ui.radio_value(&mut next_to, false, l.t("Folder:", "В папку:")).clicked()
                || ui.button(l.t("Choose…", "Выбрать…")).clicked())
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                s.output_dir = Some(dir);
            }
        });
        if let Some(dir) = &s.output_dir {
            ui.label(RichText::new(dir.display().to_string()).small().monospace())
                .on_hover_text(dir.display().to_string());
        }
        let folder_hint = match (l, &s.output_dir, s.overwrite) {
            (_, None, true) => l.t("Files are replaced in place.", "Файлы заменяются на месте.").to_string(),
            (Lang::En, None, false) => format!(
                "Added folders are saved to a \"<name>{MIRROR_SUFFIX}\" folder next to them, with all subfolders."
            ),
            (Lang::Ru, None, false) => {
                format!("Добавленные папки сохраняются рядом, в «<имя>{MIRROR_SUFFIX}», со всеми подпапками.")
            }
            (_, Some(_), _) => l
                .t("Added folders keep their subfolders inside it.", "Подпапки добавленных папок сохраняются.")
                .to_string(),
        };
        ui.label(RichText::new(folder_hint).small().weak());
        ui.checkbox(&mut s.overwrite, l.t("Overwrite existing files", "Перезаписывать файлы"))
            .on_hover_text(l.t(
                "Off: adds a -min suffix instead of replacing a file.",
                "Выкл.: к имени добавляется -min вместо замены файла.",
            ));
        ui.checkbox(
            &mut s.recursive,
            l.t("Include subfolders when adding folders", "Включать подпапки при добавлении папок"),
        );
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let l = self.settings.lang;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                if ui.add(egui::Button::new(l.t("Cancel", "Отмена")).min_size([110.0, 32.0].into())).clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                let total = job.rows.len();
                let partial: f32 = job.rows.iter().filter_map(|&r| self.rows[r].progress).sum();
                let progress = (job.done as f32 + partial) / total.max(1) as f32;
                let secs = job.started.elapsed().as_secs_f32();
                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{} / {}  ·  {secs:.1}{}", job.done, total, l.t("s", " с")))
                        .animate(true),
                );
            } else {
                let pending = self.rows.iter().filter(|r| !matches!(r.status, Status::Done(_))).count();
                let label = match (pending, self.rows.len(), l) {
                    (_, 0, _) => l.t("Convert", "Обработать").to_string(),
                    (0, n, Lang::En) => format!("Convert {n} again"),
                    (0, n, Lang::Ru) => format!("Обработать снова ({n})"),
                    (p, _, Lang::En) => format!("Convert {}", l.files(p)),
                    (p, _, Lang::Ru) => format!("Обработать {}", l.files(p)),
                };
                let button = egui::Button::new(RichText::new(label).strong()).min_size([180.0, 32.0].into());
                if ui.add_enabled(!self.rows.is_empty(), button).clicked() {
                    self.start(ui.ctx());
                }
                if let Some(t) = self.last_run {
                    let failed = self.rows.iter().filter(|r| matches!(r.status, Status::Failed { .. })).count();
                    let secs = t.as_secs_f32();
                    let mut text = match l {
                        Lang::En => format!("Finished in {secs:.2}s"),
                        Lang::Ru => format!("Готово за {secs:.2} с"),
                    };
                    if failed > 0 {
                        text += &match l {
                            Lang::En => format!(", {failed} failed"),
                            Lang::Ru => format!(", ошибок: {failed}"),
                        };
                    }
                    ui.label(RichText::new(text).color(if failed > 0 { BAD } else { GOOD }));
                }
            }
        });
        ui.add_space(6.0);
    }

    fn file_table(&mut self, ui: &mut egui::Ui) {
        let l = self.settings.lang;
        if self.rows.is_empty() {
            ui.centered_and_justified(|ui| {
                let title = l.t("Drop photos, videos or folders here", "Перетащите сюда фото, видео или папки");
                ui.label(
                    RichText::new(format!(
                        "{title}\n\nJPEG · PNG · WebP · AVIF · HEIC · TIFF · JPEG XL · SVG · RAW\nMP4 · MOV · MKV · WebM · AVI …"
                    ))
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
            .column(Column::exact(270.0).clip(true))
            .column(Column::exact(70.0))
            .column(Column::exact(72.0))
            .header(22.0, |mut header| {
                let titles = [
                    l.t("File", "Файл"),
                    l.t("Size", "Размер"),
                    l.t("Result", "Результат"),
                    l.t("Saved", "Экономия"),
                    "",
                ];
                for title in titles {
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
                        ui.label(r.input.display_path().display().to_string())
                            .on_hover_text(r.input.path.display().to_string());
                    });
                    row.col(|ui| {
                        ui.label(human(l, r.size));
                    });
                    match &r.status {
                        Status::Queued => {
                            row.col(|ui| match r.progress {
                                Some(p) if running => {
                                    ui.add(egui::ProgressBar::new(p).desired_width(200.0).show_percentage());
                                }
                                _ if running => {
                                    ui.spinner();
                                }
                                _ => {
                                    ui.label(RichText::new(l.t("Waiting", "Ожидание")).weak());
                                }
                            });
                            row.col(|_| {});
                        }
                        Status::Done(rep) => {
                            row.col(|ui| {
                                let mut text = format!("{} · {}", rep.output_format, human(l, rep.output_size));
                                if rep.kept_original {
                                    text += l.t(" (original)", " (оригинал)");
                                }
                                let resp = ui.label(text);
                                let secs = rep.elapsed.as_secs_f64();
                                let mut tip = format!(
                                    "{}\n{}×{} · {secs:.2}{}",
                                    rep.output.display(),
                                    rep.width,
                                    rep.height,
                                    l.t("s", " с")
                                );
                                if let Some(q) = rep.quality_used {
                                    tip += &format!("\n{}: {q}", l.t("Quality used", "Использованное качество"));
                                }
                                if rep.kept_original {
                                    tip += "\n";
                                    tip += l.t(
                                        "Re-encoding didn't make it smaller, so the original was copied.",
                                        "После сжатия файл не стал меньше, поэтому сохранён оригинал.",
                                    );
                                }
                                if rep.target_missed {
                                    tip += "\n";
                                    tip += l.t("Couldn't reach the target size.", "Не удалось уложиться в заданный размер.");
                                }
                                resp.on_hover_text(tip);
                            });
                            row.col(|ui| {
                                let change = rep.change_percent();
                                let color = if rep.target_missed || change > 0.0 { WARN } else { GOOD };
                                ui.label(RichText::new(format!("{change:+.0}%")).color(color));
                            });
                        }
                        Status::Failed { kind, detail } => {
                            row.col(|ui| {
                                ui.label(RichText::new(l.error(*kind)).color(BAD)).on_hover_text(detail);
                            });
                            row.col(|_| {});
                        }
                    }
                    row.col(|ui| {
                        if let Status::Done(rep) = &r.status {
                            if ui
                                .small_button(l.t("Open", "Показать"))
                                .on_hover_text(l.t("Show in folder", "Показать в папке"))
                                .clicked()
                            {
                                reveal(&rep.output);
                            }
                        } else if !running && ui.small_button("×").on_hover_text(l.t("Remove", "Убрать")).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            });
        if let Some(i) = remove {
            let row = self.rows.remove(i);
            self.known.remove(&row.input.path);
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
            .exact_size(320.0)
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

fn format_hint(l: Lang, format: Option<OutputFormat>, lossless: bool) -> Option<&'static str> {
    Some(match format? {
        OutputFormat::Jpeg => l.t(
            "mozjpeg: smaller JPEGs at the same quality.",
            "mozjpeg: JPEG меньше при том же качестве.",
        ),
        OutputFormat::Png if lossless => l.t("oxipng: lossless optimization.", "oxipng: оптимизация без потерь."),
        OutputFormat::Png => l.t(
            "Reduces to 256 colors with dithering (like pngquant); falls back to lossless if quality can't be met.",
            "Сокращает палитру до 256 цветов с дизерингом (как pngquant); если качество недостижимо - сжимает без потерь.",
        ),
        OutputFormat::Webp => l.t(
            "libwebp. Lossless works well for graphics and screenshots.",
            "libwebp. Без потерь хорошо подходит для графики и скриншотов.",
        ),
        OutputFormat::Avif => l.t(
            "Best compression for photos, but the slowest encoder.",
            "Лучшее сжатие для фото, но самый медленный кодировщик.",
        ),
        OutputFormat::Gif => l.t("256 colors, first frame only.", "256 цветов, только первый кадр."),
        OutputFormat::Ico => l.t("Scaled down to fit 256×256.", "Уменьшается до 256×256."),
        OutputFormat::Tiff => l.t("Lossless, Deflate-compressed.", "Без потерь, сжатие Deflate."),
        _ => return None,
    })
}

fn codec_hint(l: Lang, codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => l.t("Plays everywhere. Largest files.", "Воспроизводится везде. Самые большие файлы."),
        VideoCodec::H265 => l.t(
            "About half the size of H.264 at the same quality. Plays on modern devices.",
            "Примерно вдвое меньше H.264 при том же качестве. Поддерживается современными устройствами.",
        ),
        VideoCodec::Av1 => l.t(
            "Smallest files, slowest to encode. Needs newer devices to play.",
            "Самые маленькие файлы, но самое долгое сжатие. Нужны новые устройства.",
        ),
    }
}

fn human(l: Lang, bytes: u64) -> String {
    let [kb, mb, gb, b] = match l {
        Lang::En => ["KB", "MB", "GB", "B"],
        Lang::Ru => ["КБ", "МБ", "ГБ", "Б"],
    };
    match bytes {
        n if n >= 1 << 30 => format!("{:.2} {gb}", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.2} {mb}", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} {kb}", n as f64 / 1024.0),
        n => format!("{n} {b}"),
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
