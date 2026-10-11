use cxx_qt_build::{CxxQtBuilder, QmlModule};

const QML_FILES: &[&str] = &[
    "qml/Main.qml",
    "qml/EditorPane.qml",
    "qml/DocumentsPane.qml",
    "qml/DetailsPane.qml",
    "qml/PreferencesDialog.qml",
    "qml/NoticeBar.qml",
];

/// Asks qmake for Qt's QML install directory, used at runtime to detect optional styles.
fn qt_install_qml() -> String {
    let qmake = std::env::var("QMAKE").unwrap_or_else(|_| "qmake6".to_owned());
    std::process::Command::new(&qmake)
        .args(["-query", "QT_INSTALL_QML"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "/usr/lib64/qt6/qml".to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=QMAKE");
    println!(
        "cargo:rustc-env=BATSCRIBE_QT_INSTALL_QML={}",
        qt_install_qml()
    );
    for f in QML_FILES {
        println!("cargo:rerun-if-changed={f}");
    }

    let builder =
        CxxQtBuilder::new_qml_module(QmlModule::new("net.batkin.batscribe").qml_files(QML_FILES))
            .qt_module("Quick")
            .qt_module("QuickControls2")
            .qt_module("Widgets")
            .files(["src/backend.rs"])
            .cpp_files(["cpp/texthelper.h", "cpp/texthelper.cpp"]);
    // SAFETY: only adds compiler flags; Qt 6.11 headers trip GCC 16's -Wsfinae-incomplete.
    let builder = unsafe {
        builder.cc_builder(|cc| {
            cc.include("cpp");
            cc.flag_if_supported("-Wno-sfinae-incomplete");
        })
    };
    builder.build();
}
