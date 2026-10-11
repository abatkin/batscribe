mod backend;

use std::path::{Path, PathBuf};

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QQuickStyle, QString, QUrl};
use cxx_qt_lib_extras::QApplication;

const APP_ID: &str = "net.batkin.batscribe";
const KDE_STYLE: &str = "org.kde.desktop";

/// QML import directories to probe for the KDE style.
fn qml_import_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["QML_IMPORT_PATH", "QML2_IMPORT_PATH"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .collect();
    dirs.push(PathBuf::from(env!("BATSCRIBE_QT_INSTALL_QML")));
    dirs
}

fn has_qml_module(uri: &str) -> bool {
    let rel: PathBuf = uri.split('.').collect();
    qml_import_dirs()
        .iter()
        .any(|d| Path::new(d).join(&rel).join("qmldir").is_file())
}

/// Uses the KDE desktop style when installed, otherwise Fusion. An explicit
/// `QT_QUICK_CONTROLS_STYLE` or `-style` argument still takes precedence.
fn choose_style() {
    let explicit = std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_some()
        || std::env::args().any(|a| a == "-style" || a.starts_with("-style="));
    if !explicit {
        let style = if has_qml_module(KDE_STYLE) {
            KDE_STYLE
        } else {
            "Fusion"
        };
        QQuickStyle::set_style(&QString::from(style));
    }
    QQuickStyle::set_fallback_style(&QString::from("Fusion"));
}

fn main() {
    // QApplication (not QGuiApplication) is required by the QStyle-based KDE desktop style.
    let mut app = QApplication::new();
    if let Some(mut app) = app.as_mut() {
        app.as_mut()
            .set_application_name(&QString::from("batscribe"));
        app.as_mut()
            .set_application_display_name(&QString::from("Batscribe"));
        app.as_mut()
            .set_application_version(&QString::from(env!("CARGO_PKG_VERSION")));
        app.as_mut()
            .set_organization_domain(&QString::from("batkin.net"));
        // QSettings (window layout) is stored under the organization and application names.
        app.as_mut()
            .set_organization_name(&QString::from("batscribe"));
    }
    // Sets the Wayland app_id so the compositor matches the window to a desktop entry.
    QGuiApplication::set_desktop_file_name(&QString::from(APP_ID));
    choose_style();

    // Start background workers before any QML can call into the backend.
    let _ = backend::runtime();

    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/net/batkin/batscribe/qml/Main.qml"));
    }
    let code = app.as_mut().map_or(1, |app| app.exec());
    // Drop QML (and the backend) before the process exits.
    drop(engine);
    std::process::exit(code);
}
