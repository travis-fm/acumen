use glob::glob;
use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout};
use tokio::sync::Mutex;
use tokio::task::{self, spawn_blocking, JoinHandle};
use std::fs::read_to_string;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::sync::{Arc};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Stylize,
    symbols::border,
    text::{Line, Text},
    widgets::{Block, Paragraph, Widget},
    DefaultTerminal,
    Frame
};

use tokio::fs::read_to_string as read_to_string_async;

#[derive(PartialEq, Debug)]
enum SensorType {
    Chip,
    Temperature,
    Voltage,
    Current,
    Power,
    Energy,
    Humidity,
    Fan,
    Unknown,
}

#[derive(Debug)]
struct Sensor {
    display_name: String,
    file_name: String,
    input_file_path: PathBuf,
    sensor_type: SensorType,
    value: i32,
}

#[derive(Debug)]
struct HwMon {
    display_name: String,
    sensors: Vec<Sensor>,
    hwmon_path: PathBuf,
}

impl Sensor {
    fn new(value_path: PathBuf) -> Self {
        let file_name = value_path
            .file_name()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default()
            .to_owned();
        let display_name = file_name.split("_").next().unwrap_or_default().to_owned();

        Sensor {
            sensor_type: Sensor::parse_type_from_file(&display_name),
            file_name,
            display_name,
            value: 0,
            input_file_path: value_path,
        }
    }

    fn parse_type_from_file(file_name: &str) -> SensorType {
        match file_name.split(char::is_numeric).next() {
            Some(name) => match name {
                "chip" => SensorType::Chip,
                "temp" => SensorType::Temperature,
                "in" => SensorType::Voltage,
                "curr" => SensorType::Current,
                "power" => SensorType::Power,
                "energy" => SensorType::Energy,
                "humidity" => SensorType::Humidity,
                "fan" => SensorType::Fan,
                _ => SensorType::Unknown,
            },
            None => SensorType::Unknown,
        }
    }
}
impl Widget for &Sensor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [name_area, value_area] = Layout::horizontal([
            Constraint::Percentage(25),
            Constraint::Percentage(75),
        ]).areas(area);

        let render_name = Line::from(self.display_name.as_str());
        let render_curr_value = Line::from(self.value.to_string());

        Paragraph::new(render_name)
            .alignment(Alignment::Left)
            .render(name_area, buf);
        Paragraph::new(render_curr_value)
            .alignment(Alignment::Right)
            .render(value_area, buf);
    }
}

impl HwMon {
    async fn new(hwmon_path: PathBuf) -> io::Result<Self> {
        let hwmon = HwMon {
            display_name: read_to_string_async(hwmon_path.join("name")).await?
                .trim_ascii()
                .to_string(),
            sensors: HwMon::init_sensors(&hwmon_path).await?,
            hwmon_path,
        };

        Ok(hwmon)
    }

    async fn init_sensors(hwmon_path: &Path) -> io::Result<Vec<Sensor>> {
        let mut sensors: Vec<Sensor> = vec![];

        let string_parse_err = io::Error::other(format!(
            "Could not parse string from path: {:#?}",
            hwmon_path
        ));
        let glob_path = hwmon_path.to_str().ok_or(string_parse_err)?.to_owned() + "/*_input";

        match glob(&glob_path) {
            Ok(paths) => {
                for path in paths {
                    match path {
                        Ok(file) => {
                            let sensor_exists = sensors.iter().any(|s| s.input_file_path == file);

                            if sensor_exists {
                                continue;
                            } else {
                                sensors.push(Sensor::new(file));
                            }
                        }
                        Err(e) => {
                            io::Error::other(e);
                        }
                    }
                }
            }
            Err(e) => {
                io::Error::other(e);
            }
        }

        Ok(sensors)
    }

    fn update_sensors(&mut self) {
        for sensor in &mut self.sensors {
            sensor.value = read_to_string(&sensor.input_file_path)
                .unwrap_or_default()
                .trim_ascii()
                .parse::<i32>()
                .unwrap_or_default();
        }
    }
}
impl Widget for &HwMon {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let module_name = Line::from(self.display_name.as_str());
        let module_block = Block::bordered()
            .border_set(border::PLAIN)
            .title(module_name.centered());
        let mut constraints = vec![];
        self.sensors.iter().for_each(|_| constraints.push(Constraint::Fill(1)));

        let layout = Layout::vertical(constraints).split(module_block.inner(area));

        module_block.render(area, buf);
        for i in 0..self.sensors.len() {
            self.sensors[i].render(layout[i], buf);
        }
    }
}

#[derive(Debug)]
struct App {
    exit: bool,
    modules: Vec<Arc<Mutex<HwMon>>>,
    // modules: Vec<HwMon>,
    sensor_refresh_interval: Duration,
    app_frame_rate: Duration,
    last_sensor_refresh: Instant,
}

impl App {
    async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            self.update_modules().await;
            terminal.draw(|f| self.draw(f))?;
            self.handle_events()?;
        }

        Ok(())
    }

    async fn update_modules(&mut self) {
        if Instant::now() >= self.last_sensor_refresh + self.sensor_refresh_interval {
            let mut tasks = vec![];

            for module in &self.modules {
                let module = module.clone();

                tasks.push(tokio::spawn(async move {
                    module.lock().await.update_sensors()
                }));
            }

            for task in tasks {
                task.await;
            }

            self.last_sensor_refresh = Instant::now();
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn exit(&mut self) {
        self.exit = true;
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Char('q') => self.exit(),
            _ => {}
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if event::poll(Duration::from_secs(0))? {
            match event::read()? {
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    self.handle_key_event(key_event)
                }

                _ => {}
            };
        }
        
        Ok(())
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let app_title = Line::from("Acumen Hardware Monitor");
        let app_version = Line::from("v0.0.1-dev");
        let app_block = Block::bordered()
            .title(app_title.centered())
            .title_bottom(app_version.right_aligned())
            .border_set(border::THICK);

        // let header_footer_size = 16;
        // let main_area_size = app_block.inner(area).height - (header_footer_size * 2);
        // let [header_area, main_area, footer_area] = Layout::vertical([
        //     Constraint::Max(header_footer_size),
        //     Constraint::Length(main_area_size),
        //     Constraint::Max(header_footer_size),
        // ]).areas(app_block.inner(area));
        let [main_area] = Layout::vertical([
            Constraint::Fill(1)
        ]).areas(app_block.inner(area));

        let module_col_size = 100 / if self.modules.len() > 0 { self.modules.len() } else { 1 };
        let module_cols = (0..self.modules.len())
            .map(|_| Constraint::Percentage(module_col_size as u16));
        let module_layout = Layout::horizontal(module_cols).spacing(1).split(main_area);

        app_block.render(area, buf);
        
        for i in 0..self.modules.len() {
            self.modules[i].blocking_lock().render(module_layout[i], buf);
        }
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut app = App {
        exit: false,
        modules: vec![],
        sensor_refresh_interval: Duration::from_millis(1000),
        app_frame_rate: Duration::from_secs_f64(1.0/60.0),
        last_sensor_refresh: Instant::now(),
    };
    
    match glob("/sys/class/hwmon/hwmon*") {
        Ok(paths) => {
            let mut tasks = vec![];

            for path in paths.flatten() {
                tasks.push(tokio::spawn(async move {
                    HwMon::new(path.clone()).await
                }));
            }

            for task in tasks {
                app.modules.push(Arc::new(Mutex::new(task.await??)));
            }
        }
        Err(..) => {
            println!("Unable to read glob pattern");
        }
    }
    
    let mut terminal = ratatui::init();
    let app_result = app.run(&mut terminal).await;
    ratatui::restore();
    app_result
}
