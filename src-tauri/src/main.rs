// Prevent a second window from opening a second writer on the same DB.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kiri_lib::run()
}
