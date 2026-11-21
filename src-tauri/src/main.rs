// LinFix — лаунчер игр FreeTP / Online-Fix для Linux
// Copyright (C) 2026  LinFix contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    linfix_lib::run()
}
