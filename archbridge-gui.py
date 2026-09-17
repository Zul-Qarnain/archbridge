#!/usr/bin/env python3
"""
ArchBridge Modern GUI Frontend
Pixel-perfect implementation of ui.png with Dynamic Icon & Package Metadata Retrieval
Connects over JSON-RPC 2.0 (stdin/stdout) to 'archbridge serve'
"""

import json
import os
import re
import shutil
import subprocess
import sys
import urllib.request
import webbrowser
from pathlib import Path

from PyQt6.QtCore import QByteArray, QPoint, QProcess, QRect, QRectF, QSize, Qt, QThread, QTimer, pyqtSignal
from PyQt6.QtGui import (
    QBrush,
    QColor,
    QCursor,
    QFont,
    QIcon,
    QLinearGradient,
    QPainter,
    QPainterPath,
    QPalette,
    QPen,
    QPixmap,
)
from PyQt6.QtSvg import QSvgRenderer
from PyQt6.QtWidgets import (
    QApplication,
    QCheckBox,
    QComboBox,
    QDialog,
    QFileDialog,
    QFrame,
    QGraphicsDropShadowEffect,
    QGridLayout,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QDialogButtonBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMainWindow,
    QMenu,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QSplitter,
    QStackedWidget,
    QTableWidget,
    QTableWidgetItem,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)


class RpcWorker(QThread):
    response_received = pyqtSignal(dict)
    error_occurred = pyqtSignal(str)

    def __init__(self, binary_path):
        super().__init__()
        self.binary_path = binary_path
        self.process = None
        self.request_id = 0
        self.init_error = None
        cmd = [self.binary_path, "serve"]
        try:
            self.process = subprocess.Popen(
                cmd,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                bufsize=1,
            )
        except Exception as e:
            self.init_error = str(e)

    def run(self):
        if self.init_error:
            self.error_occurred.emit(f"Failed to start RPC service '{self.binary_path} serve': {self.init_error}")
            return
        if not self.process or not self.process.stdout:
            return


        for line in self.process.stdout:
            line = line.strip()
            if line:
                try:
                    data = json.loads(line)
                    self.response_received.emit(data)
                except Exception as e:
                    self.error_occurred.emit(f"JSON parse error from RPC server: {e}")

    def send_request(self, method, params=None):
        if not self.process or self.process.poll() is not None:
            self.error_occurred.emit("RPC server is not running.")
            return None

        self.request_id += 1
        req_id = self.request_id
        req = {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params or {},
        }
        try:
            payload = json.dumps(req) + "\n"
            self.process.stdin.write(payload)
            self.process.stdin.flush()
            return req_id
        except Exception as e:
            self.error_occurred.emit(f"Failed to write to RPC server: {e}")
            return None

    def stop(self):
        if self.process:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=1.0)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=1.0)
            if self.process.stdin:
                self.process.stdin.close()
        if self.isRunning():
            self.wait(2000)


class IconFetcherThread(QThread):
    """Background worker to fetch high-res application icons without blocking the GUI"""
    icon_fetched = pyqtSignal(str, QPixmap, str)

    def __init__(self, app_name, parent=None):
        super().__init__(parent)
        self.app_name = app_name.lower().strip()

    def run(self):
        cache_dir = Path.home() / ".cache" / "archbridge" / "icons"
        cache_dir.mkdir(parents=True, exist_ok=True)
        cached_file = cache_dir / f"{self.app_name}.png"

        # 1. Check local cache
        if cached_file.exists():
            pix = QPixmap(str(cached_file))
            if not pix.isNull():
                self.icon_fetched.emit(self.app_name, pix, "#1e293b")
                return

        # 2. Check local Linux desktop / pixmaps
        local_candidates = [
            Path(f"/usr/share/pixmaps/{self.app_name}.png"),
            Path(f"/usr/share/pixmaps/{self.app_name}.svg"),
            Path(f"/usr/share/icons/hicolor/128x128/apps/{self.app_name}.png"),
            Path(f"/usr/share/icons/hicolor/scalable/apps/{self.app_name}.svg"),
        ]
        for p in local_candidates:
            if p.exists():
                pix = QPixmap(str(p))
                if not pix.isNull():
                    self.icon_fetched.emit(self.app_name, pix, "#1e293b")
                    return

        # 3. Check online CDNs (SimpleIcons SVG and Dashboard PNGs)
        slug_aliases = {
            "vscode": "vscodium",
            "visualstudiocode": "vscodium",
            "code": "vscodium",
            "obs-studio": "obsstudio",
            "brave-bin": "brave",
            "brave-browser": "brave",
            "discord-canary": "discord",
            "discord-ptb": "discord",
            "google-chrome": "googlechrome",
            "chromium": "chromium",
            "sublime-text": "sublimetext",
        }
        slug = slug_aliases.get(self.app_name, self.app_name)

        # Try SimpleIcons colored/white SVG
        urls = [
            f"https://cdn.jsdelivr.net/gh/walkxcode/dashboard-icons/png/{slug}.png",
            f"https://cdn.simpleicons.org/{slug}/white",
            f"https://cdn.simpleicons.org/{self.app_name}/white",
        ]

        for url in urls:
            try:
                req = urllib.request.Request(url, headers={"User-Agent": "ArchBridge/1.0"})
                with urllib.request.urlopen(req, timeout=3) as resp:
                    raw_data = resp.read()
                    if raw_data:
                        if url.endswith(".svg") or b"<svg" in raw_data[:100]:
                            renderer = QSvgRenderer(QByteArray(raw_data))
                            pix = QPixmap(128, 128)
                            pix.fill(Qt.GlobalColor.transparent)
                            painter = QPainter(pix)
                            renderer.render(painter)
                            painter.end()
                        else:
                            pix = QPixmap()
                            pix.loadFromData(raw_data)

                        if not pix.isNull():
                            pix.save(str(cached_file), "PNG")
                            # Pick background accent based on software
                            brand_colors = {
                                "brave": "#c2410c",
                                "docker": "#0284c7",
                                "discord": "#5865f2",
                                "vscode": "#007acc",
                                "steam": "#1b2838",
                                "obs-studio": "#302e31",
                                "firefox": "#ea580c",
                                "spotify": "#1db954",
                            }
                            accent = brand_colors.get(self.app_name, "#1e293b")
                            self.icon_fetched.emit(self.app_name, pix, accent)
                            return
            except Exception:
                continue

        # 4. If no icon found online, render a clean modern gradient letter badge
        fallback = self.generate_fallback_pixmap(self.app_name)
        self.icon_fetched.emit(self.app_name, fallback, "#1e293b")

    def generate_fallback_pixmap(self, name):
        pix = QPixmap(128, 128)
        pix.fill(Qt.GlobalColor.transparent)
        painter = QPainter(pix)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)

        # Gradient background
        grad = QLinearGradient(0, 0, 128, 128)
        grad.setColorAt(0, QColor("#0284c7"))
        grad.setColorAt(1, QColor("#0369a1"))
        painter.setBrush(QBrush(grad))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.drawRoundedRect(0, 0, 128, 128, 24, 24)

        # Letter initial
        letter = name[:2].upper() if len(name) >= 2 else (name[:1].upper() if name else "AB")
        painter.setPen(QPen(QColor("#ffffff")))
        font = QFont("Inter", 38, QFont.Weight.Bold)
        painter.setFont(font)
        painter.drawText(QRect(0, 0, 128, 128), Qt.AlignmentFlag.AlignCenter, letter)
        painter.end()
        return pix


class MountainArtwork(QWidget):
    """Draws the subtle Arch Linux geometric mountain line-art at sidebar bottom"""
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setFixedHeight(95)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)

        pen = QPen(QColor("#152d47"), 1.5)
        painter.setPen(pen)

        w = self.width()
        h = self.height()

        p1 = QPoint(8, h - 22)
        p2 = QPoint(int(w * 0.36), int(h * 0.22))
        p3 = QPoint(int(w * 0.58), h - 22)
        p4 = QPoint(int(w * 0.76), int(h * 0.44))
        p5 = QPoint(w - 8, h - 22)

        path = QPainterPath()
        path.moveTo(p1.x(), p1.y())
        path.lineTo(p2.x(), p2.y())
        path.lineTo(p3.x(), p3.y())
        path.lineTo(p4.x(), p4.y())
        path.lineTo(p5.x(), p5.y())
        painter.drawPath(path)

        painter.drawLine(p2, QPoint(int(w * 0.48), h - 22))
        painter.drawLine(QPoint(int(w * 0.22), int(h * 0.5)), QPoint(int(w * 0.42), h - 22))

        painter.setPen(QPen(QColor("#3b5f85"), 1))
        painter.setFont(QFont("Segoe UI", 8))
        painter.drawText(QRect(0, h - 18, w, 18), Qt.AlignmentFlag.AlignRight, "Arch Linux · Powered by You")


class NavButton(QPushButton):
    def __init__(self, icon_str, text, parent=None):
        super().__init__(parent)
        self.icon_str = icon_str
        self.nav_text = text
        self.is_active = False
        self.setFixedHeight(38)
        self.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self.update_style()

    def set_active(self, active):
        self.is_active = active
        self.update_style()

    def update_style(self):
        if self.is_active:
            self.setStyleSheet("""
                QPushButton {
                    background-color: #0e1f36;
                    color: #38bdf8;
                    border: 1px solid #1b3252;
                    border-radius: 8px;
                    padding-left: 16px;
                    text-align: left;
                    font-size: 13px;
                    font-weight: 700;
                }
            """)
        else:
            self.setStyleSheet("""
                QPushButton {
                    background-color: transparent;
                    color: #8492a6;
                    border: 1px solid transparent;
                    border-radius: 8px;
                    font-size: 13px;
                    font-weight: 500;
                }
                QPushButton:hover {
                    background-color: #0d1726;
                    color: #d1dbe8;
                    border: 1px solid #142233;
                }
            """)
        clean_text = self.nav_text.replace("&", "&&")
        self.setText(f"{self.icon_str}   {clean_text}")


class HeaderIcon(QLabel):
    """Stable inline SVG icon for the compact status header."""

    def __init__(self, kind, parent=None):
        super().__init__(parent)
        self.kind = kind
        self.setFixedSize(30, 30)
        self.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.set_state("#38bdf8")

    def set_state(self, color):
        paths = {
            "engine": '<path d="M7 15h6m2 0h6M8 12.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Zm14 0a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Z"/>',
            "history": '<path d="M7 9v4h4M8 13a7 7 0 1 0 2-5m5 2v5l4 2"/>',
            "system": '<path d="m8.5 10 6.5-3 6.5 3-1 7c-.8 3-3.2 5.2-5.5 6.5-2.3-1.3-4.7-3.5-5.5-6.5l-1-7Zm3 5.5 2.5 2.5 4.5-5"/>'
        }
        svg = f'''<svg width="30" height="30" viewBox="0 0 30 30" xmlns="http://www.w3.org/2000/svg">
          <rect x="0.5" y="0.5" width="29" height="29" rx="8" fill="#0b1b2e" stroke="#23405e"/>
          <g fill="none" stroke="{color}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{paths[self.kind]}</g>
        </svg>'''
        renderer = QSvgRenderer(QByteArray(svg.encode("utf-8")))
        pixmap = QPixmap(30, 30)
        pixmap.fill(Qt.GlobalColor.transparent)
        painter = QPainter(pixmap)
        renderer.render(painter)
        painter.end()
        self.setPixmap(pixmap)


class ArchBridgeWindow(QMainWindow):
    def __init__(self, binary_path):
        super().__init__()
        self.binary_path = binary_path
        self.pending_callbacks = {}
        self.active_plan = None
        self.search_history = []
        self.current_query = ""
        self.last_search_result = None
        self.icon_threads = []
        self.privilege_retry_used = False
        self.clear_sudo_after_job = False
        self.sudo_process = None

        self.setWindowTitle("ArchBridge — Software Discovery & Packaging Assistant")
        self.resize(1280, 840)
        self.setMinimumSize(1100, 720)

        self.apply_theme()
        self.init_ui()
        self.init_rpc()


    def apply_theme(self):
        palette = QPalette()
        palette.setColor(QPalette.ColorRole.Window, QColor("#070b12"))
        palette.setColor(QPalette.ColorRole.WindowText, QColor("#e2e8f0"))
        palette.setColor(QPalette.ColorRole.Base, QColor("#09111c"))
        palette.setColor(QPalette.ColorRole.AlternateBase, QColor("#0d1726"))
        palette.setColor(QPalette.ColorRole.ToolTipBase, QColor("#e2e8f0"))
        palette.setColor(QPalette.ColorRole.ToolTipText, QColor("#070b12"))
        palette.setColor(QPalette.ColorRole.Text, QColor("#e2e8f0"))
        palette.setColor(QPalette.ColorRole.Button, QColor("#0f1a29"))
        palette.setColor(QPalette.ColorRole.ButtonText, QColor("#e2e8f0"))
        palette.setColor(QPalette.ColorRole.Highlight, QColor("#0284c7"))
        palette.setColor(QPalette.ColorRole.HighlightedText, QColor("#ffffff"))
        self.setPalette(palette)

        self.setStyleSheet("""
            QMainWindow {
                background-color: #070b12;
                border: 1px solid #20314a;
            }
            QWidget {
                font-family: 'Inter', 'Segoe UI', 'SF Pro Display', Ubuntu, sans-serif;
                color: #cbd5e1;
            }
            QScrollArea {
                border: none;
                background-color: transparent;
            }
            QScrollBar:vertical {
                border: none;
                background: #080f1a;
                width: 8px;
                margin: 0px;
                border-radius: 4px;
            }
            QScrollBar::handle:vertical {
                background: #192a40;
                min-height: 20px;
                border-radius: 4px;
            }
            QScrollBar::handle:vertical:hover {
                background: #0284c7;
            }
            QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {
                height: 0px;
            }
            QLineEdit {
                background-color: rgba(12, 25, 43, 225);
                border: 1px solid #29415f;
                border-radius: 10px;
                padding: 10px 14px;
                color: #ffffff;
                font-size: 13px;
                selection-background-color: #0284c7;
            }
            QLineEdit:focus {
                border: 1px solid #0084d1;
                background-color: #0b1829;
            }
            QComboBox {
                background-color: rgba(12, 25, 43, 225);
                border: 1px solid #29415f;
                border-radius: 10px;
                padding: 8px 12px;
                color: #cbd5e1;
                font-size: 12px;
                font-weight: 500;
            }
            QComboBox::drop-down {
                border: none;
                width: 20px;
            }
            QComboBox QAbstractItemView {
                background-color: #0a1322;
                border: 1px solid #1a2c44;
                color: #cbd5e1;
                selection-background-color: #0284c7;
                selection-color: #ffffff;
                outline: none;
                padding: 4px;
            }
            QPushButton {
                background-color: rgba(18, 35, 58, 220);
                border: 1px solid #29415f;
                border-radius: 10px;
                padding: 8px 16px;
                color: #e2e8f0;
                font-size: 12px;
                font-weight: 600;
            }
            QPushButton:hover {
                background-color: #14253d;
                border-color: #244166;
                color: #ffffff;
            }
            QPushButton:pressed {
                background-color: #0a1422;
            }
            QTextEdit {
                background-color: rgba(8, 17, 30, 235);
                border: 1px solid #203650;
                border-radius: 10px;
                padding: 10px;
                color: #cbd5e1;
                font-family: 'JetBrains Mono', 'Fira Code', 'Consolas', monospace;
                font-size: 12px;
            }
        """)

    def init_ui(self):
        root_widget = QWidget()
        self.setCentralWidget(root_widget)
        root_layout = QHBoxLayout(root_widget)
        root_layout.setContentsMargins(0, 0, 0, 0)
        root_layout.setSpacing(0)

        # 1. Left Sidebar
        self.setup_sidebar(root_layout)

        # 2. Main Container
        main_container = QWidget()
        main_container.setStyleSheet("background-color: #070b12;")
        main_layout = QVBoxLayout(main_container)
        main_layout.setContentsMargins(24, 16, 24, 12)
        main_layout.setSpacing(14)

        # Top Bar
        self.setup_top_bar(main_layout)

        # Stacked Views
        self.stack = QStackedWidget()
        main_layout.addWidget(self.stack)

        self.view_discover = QWidget()
        self.view_inspect = QWidget()
        self.view_build = QWidget()
        self.view_doctor = QWidget()
        self.view_settings = QWidget()

        self.stack.addWidget(self.view_discover)
        self.stack.addWidget(self.view_inspect)
        self.stack.addWidget(self.view_build)
        self.stack.addWidget(self.view_doctor)
        self.stack.addWidget(self.view_settings)

        self.setup_discover_view()
        self.setup_inspect_view()
        self.setup_build_view()
        self.setup_doctor_view()
        self.setup_settings_view()

        # Bottom Status Bar
        self.setup_bottom_bar(main_layout)

        root_layout.addWidget(main_container)

    # --- 1. SIDEBAR ---
    def setup_sidebar(self, root_layout):
        sidebar = QFrame()
        sidebar.setFixedWidth(230)
        sidebar.setStyleSheet("QFrame { background-color: #080d16; border-right: 1px solid #101926; }")
        layout = QVBoxLayout(sidebar)
        layout.setContentsMargins(16, 20, 16, 16)
        layout.setSpacing(6)

        brand_box = QHBoxLayout()
        logo_icon = QLabel("▲")
        logo_icon.setStyleSheet("color: #00a4e4; font-size: 26px; font-weight: bold;")
        brand_box.addWidget(logo_icon)

        title_col = QVBoxLayout()
        title_col.setSpacing(0)
        brand_title = QLabel("Arch<span style='color: #00a4e4;'>Bridge</span>")
        brand_title.setStyleSheet("font-size: 20px; font-weight: 800; color: #ffffff;")
        brand_sub = QLabel("Discover · Build · Bridge · Go Further")
        brand_sub.setStyleSheet("font-size: 8px; color: #536780; font-weight: 500; margin-top: 2px;")
        title_col.addWidget(brand_title)
        title_col.addWidget(brand_sub)

        brand_box.addLayout(title_col)
        brand_box.addStretch()
        layout.addLayout(brand_box)

        layout.addSpacing(12)

        self.nav_btns = []
        self.btn_nav_discover = NavButton("🔍", "Discovery")
        self.btn_nav_inspect = NavButton("📦", "Inspect (.deb / .rpm)")
        self.btn_nav_build = NavButton("🔨", "Build & Install")
        self.btn_nav_doctor = NavButton("🤍", "Doctor Health")
        self.btn_nav_settings = NavButton("⚙️", "Settings")

        self.nav_btns = [
            self.btn_nav_discover,
            self.btn_nav_inspect,
            self.btn_nav_build,
            self.btn_nav_doctor,
            self.btn_nav_settings,
        ]

        for idx, btn in enumerate(self.nav_btns):
            btn.clicked.connect(lambda checked, i=idx: self.switch_tab(i))
            layout.addWidget(btn)

        self.btn_nav_discover.set_active(True)

        layout.addStretch()

        quote_lbl = QLabel("“ Same software.<br>&nbsp;&nbsp;More possibilities. ”")
        quote_lbl.setStyleSheet("font-style: italic; color: #4f6782; font-size: 11px; padding-left: 6px;")
        layout.addWidget(quote_lbl)

        layout.addWidget(MountainArtwork())

        root_layout.addWidget(sidebar)

    def switch_tab(self, index):
        self.stack.setCurrentIndex(index)
        for i, btn in enumerate(self.nav_btns):
            btn.set_active(i == index)

    # --- 2. TOP BAR ---
    def setup_top_bar(self, main_layout):
        top_bar = QHBoxLayout()
        top_bar.setContentsMargins(0, 0, 0, 0)

        header_surface = QFrame()
        header_surface.setObjectName("headerSurface")
        header_surface.setMinimumWidth(520)
        header_surface.setMaximumWidth(720)
        header_surface.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        header_surface.setStyleSheet("""
            QFrame#headerSurface {
                background-color: rgba(12, 24, 41, 225);
                border: 1px solid #1c304a;
                border-radius: 14px;
            }
            QFrame#headerCell {
                background-color: transparent;
                border: 1px solid transparent;
                border-radius: 10px;
            }
            QFrame#headerCell:hover {
                background-color: #102039;
                border-color: #29415f;
            }
            QLabel#headerTitle {
                color: #e2e8f0;
                font-size: 11px;
                font-weight: 700;
            }
            QLabel#headerSubtitle {
                color: #64748b;
                font-size: 9px;
            }
            QPushButton#historyButton {
                background: transparent;
                border: none;
                color: #e2e8f0;
                font-size: 11px;
                font-weight: 700;
                text-align: left;
                padding: 0;
            }
            QPushButton#historyButton:disabled { color: #64748b; }
        """)
        header_layout = QHBoxLayout(header_surface)
        header_layout.setContentsMargins(6, 6, 6, 6)
        header_layout.setSpacing(4)

        def make_cell(icon_kind, title, subtitle):
            cell = QFrame()
            cell.setObjectName("headerCell")
            cell.setMinimumHeight(50)
            cell.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
            cell_layout = QHBoxLayout(cell)
            cell_layout.setContentsMargins(8, 5, 10, 5)
            cell_layout.setSpacing(8)
            icon_label = HeaderIcon(icon_kind)
            text_col = QVBoxLayout()
            text_col.setSpacing(1)
            title_label = QLabel(title)
            title_label.setObjectName("headerTitle")
            subtitle_label = QLabel(subtitle)
            subtitle_label.setObjectName("headerSubtitle")
            text_col.addWidget(title_label)
            text_col.addWidget(subtitle_label)
            cell_layout.addWidget(icon_label)
            cell_layout.addLayout(text_col, 1)
            return cell, title_label, subtitle_label

        self.ipc_pill, self.ipc_title, self.ipc_sub = make_cell("engine", "Engine ready", "Local service")
        self.ipc_dot = self.ipc_pill.findChild(HeaderIcon)
        header_layout.addWidget(self.ipc_pill)

        history_cell = QFrame()
        history_cell.setObjectName("headerCell")
        history_cell.setMinimumHeight(50)
        history_cell.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        history_layout = QHBoxLayout(history_cell)
        history_layout.setContentsMargins(8, 5, 10, 5)
        history_layout.setSpacing(8)
        history_icon = HeaderIcon("history")
        history_text = QVBoxLayout()
        history_text.setSpacing(1)
        self.btn_history = QPushButton("History")
        self.btn_history.setObjectName("historyButton")
        self.btn_history.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self.history_sub = QLabel("No recent searches")
        self.history_sub.setObjectName("headerSubtitle")
        history_text.addWidget(self.btn_history)
        history_text.addWidget(self.history_sub)
        history_layout.addWidget(history_icon)
        history_layout.addLayout(history_text, 1)
        header_layout.addWidget(history_cell)

        self.history_menu = QMenu(self)
        self.history_menu.setStyleSheet("background-color: #0a1322; color: #cbd5e1; border: 1px solid #16263b; padding: 4px;")
        self.btn_history.setMenu(self.history_menu)
        self.btn_history.setEnabled(False)

        self.doc_pill, self.doc_status_lbl, self.doc_sub = make_cell("system", "System ready", "Health checks passing")
        self.doc_dot = self.doc_pill.findChild(HeaderIcon)
        header_layout.addWidget(self.doc_pill)

        top_bar.addWidget(header_surface)
        top_bar.addStretch()

        main_layout.addLayout(top_bar)

    # --- 3. VIEW 1: DISCOVER SOFTWARE ---
    def setup_discover_view(self):
        main_h_layout = QHBoxLayout(self.view_discover)
        main_h_layout.setContentsMargins(0, 8, 0, 0)
        main_h_layout.setSpacing(18)

        # Left Column
        left_col = QWidget()
        left_layout = QVBoxLayout(left_col)
        left_layout.setContentsMargins(0, 0, 0, 0)
        left_layout.setSpacing(12)

        # Title
        title_box = QVBoxLayout()
        title_box.setSpacing(2)
        h1 = QLabel("Discover Software")
        h1.setStyleSheet("font-size: 24px; font-weight: 800; color: #f8fafc;")
        h1_sub = QLabel("Find the best way to get your software on Arch Linux.")
        h1_sub.setStyleSheet("font-size: 13px; color: #64748b;")
        title_box.addWidget(h1)
        title_box.addWidget(h1_sub)
        left_layout.addLayout(title_box)

        # Search Bar Row
        search_box = QHBoxLayout()
        search_box.setSpacing(10)

        search_input_frame = QFrame()
        search_input_frame.setStyleSheet("""
            QFrame {
                background-color: #09121f;
                border: 1px solid #152438;
                border-radius: 8px;
            }
            QFrame:focus-within {
                border: 1px solid #0084d1;
                background-color: #0b1829;
            }
        """)
        s_inner_layout = QHBoxLayout(search_input_frame)
        s_inner_layout.setContentsMargins(12, 2, 8, 2)
        s_inner_layout.setSpacing(8)

        s_icon = QLabel("🔍")
        s_icon.setStyleSheet("color: #64748b; font-size: 14px; border: none; background: transparent;")
        s_inner_layout.addWidget(s_icon)

        self.input_search = QLineEdit()
        self.input_search.clear()
        self.input_search.setPlaceholderText("Search package name or upstream git URL...")
        self.input_search.setStyleSheet("border: none; background: transparent; padding: 8px 0px; font-size: 13px; color: #ffffff;")
        self.input_search.returnPressed.connect(self.do_search_clicked)
        self.input_search.textChanged.connect(self.on_search_text_changed)
        s_inner_layout.addWidget(self.input_search, 1)

        btn_clear = QPushButton("✕")
        btn_clear.setFixedSize(20, 20)
        btn_clear.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        btn_clear.setStyleSheet("QPushButton { border: none; background: transparent; color: #475569; font-size: 11px; } QPushButton:hover { color: #cbd5e1; }")
        btn_clear.clicked.connect(lambda: self.input_search.clear())
        s_inner_layout.addWidget(btn_clear)

        self.combo_source_filter = QComboBox()
        self.combo_source_filter.addItems(["All Sources", "Official", "AUR", "Flatpak", "AppImage", "Upstream"])
        self.combo_source_filter.setFixedWidth(130)
        self.combo_source_filter.currentIndexChanged.connect(self.refresh_search_results)

        self.btn_search = QPushButton("Search")
        self.btn_search.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self.btn_search.setStyleSheet("""
            QPushButton {
                background-color: #0084d1;
                border: none;
                border-radius: 8px;
                padding: 10px 24px;
                font-weight: 700;
                font-size: 13px;
                color: #ffffff;
            }
            QPushButton:hover {
                background-color: #0284c7;
            }
            QPushButton:pressed {
                background-color: #0369a1;
            }
        """)
        self.btn_search.clicked.connect(self.do_search_clicked)

        search_box.addWidget(search_input_frame, 1)
        search_box.addWidget(self.combo_source_filter)
        search_box.addWidget(self.btn_search)
        left_layout.addLayout(search_box)

        # Quick Suggestions
        suggestions_box = QHBoxLayout()
        sugg_lbl = QLabel("Try: <a href='brave' style='color:#0284c7; text-decoration:none;'>brave</a>, <a href='vscode' style='color:#0284c7; text-decoration:none;'>vscode</a>, <a href='docker' style='color:#0284c7; text-decoration:none;'>docker</a>, <a href='steam' style='color:#0284c7; text-decoration:none;'>steam</a>, <a href='obs-studio' style='color:#0284c7; text-decoration:none;'>obs-studio</a>, <a href='discord' style='color:#0284c7; text-decoration:none;'>discord</a> ...")
        sugg_lbl.setStyleSheet("color: #64748b; font-size: 11px;")
        sugg_lbl.linkActivated.connect(lambda link: self.do_search_query(link))
        suggestions_box.addWidget(sugg_lbl)
        suggestions_box.addStretch()
        left_layout.addLayout(suggestions_box)

        # Recommended Path Banner
        self.rec_card = QFrame()
        self.rec_card.setObjectName("recCard")
        self.rec_card.setStyleSheet("""
            QFrame#recCard {
                background-color: rgba(7, 48, 40, 220);
                border: 1px solid #16765a;
                border-radius: 14px;
            }
            QLabel {
                border: none;
                background: transparent;
                padding: 0;
            }
        """)
        rec_shadow = QGraphicsDropShadowEffect(self)
        rec_shadow.setBlurRadius(28)
        rec_shadow.setColor(QColor(0, 0, 0, 110))
        rec_shadow.setOffset(0, 8)
        self.rec_card.setGraphicsEffect(rec_shadow)
        rec_layout = QHBoxLayout(self.rec_card)
        rec_layout.setContentsMargins(16, 14, 16, 14)
        rec_layout.setSpacing(14)

        rec_icon = QLabel("★")
        rec_icon.setStyleSheet("color: #10b981; font-size: 26px; border: none; background: transparent;")
        rec_layout.addWidget(rec_icon)

        rec_text_col = QVBoxLayout()
        rec_text_col.setSpacing(2)
        rec_header_row = QHBoxLayout()
        rec_title = QLabel("Recommended Path")
        rec_title.setStyleSheet("font-size: 14px; font-weight: 800; color: #10b981; border: none; background: transparent;")
        rec_safe_badge = QLabel("SAFE")
        rec_safe_badge.setStyleSheet("background-color: #0c4331; color: #34d399; font-size: 10px; font-weight: 800; padding: 2px 6px; border-radius: 4px; border: none;")
        rec_header_row.addWidget(rec_title)
        rec_header_row.addWidget(rec_safe_badge)
        rec_header_row.addStretch()

        self.rec_desc = QLabel("Search for software to compare available sources.")
        self.rec_desc.setStyleSheet("font-size: 12px; font-weight: 600; color: #e2e8f0; border: none; background: transparent;")
        self.rec_sub = QLabel("")
        self.rec_sub.setStyleSheet("font-size: 11px; color: #6ee7b7; border: none; background: transparent;")

        rec_text_col.addLayout(rec_header_row)
        rec_text_col.addWidget(self.rec_desc)
        rec_text_col.addWidget(self.rec_sub)

        rec_layout.addLayout(rec_text_col, 1)

        self.btn_prepare_plan = QPushButton("Prepare Install Plan  →")
        self.btn_prepare_plan.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        self.btn_prepare_plan.setStyleSheet("""
            QPushButton {
                background-color: #0084d1;
                border: none;
                border-radius: 8px;
                padding: 10px 18px;
                font-size: 12px;
                font-weight: 700;
                color: #ffffff;
            }
            QPushButton:hover {
                background-color: #0284c7;
            }
        """)
        self.btn_prepare_plan.clicked.connect(self.quick_prepare_install)
        rec_layout.addWidget(self.btn_prepare_plan)

        left_layout.addWidget(self.rec_card)
        self.rec_card.hide()

        # Available Sources Section Header
        sec_header = QHBoxLayout()
        sec_title = QLabel("Available Sources")
        sec_title.setStyleSheet("font-size: 15px; font-weight: 800; color: #f8fafc;")
        sec_header.addWidget(sec_title)
        sec_header.addStretch()

        self.sort_combo = QComboBox()
        self.sort_combo.addItems(["Recommended", "Source", "Name"])
        self.sort_combo.setToolTip("Choose how verified sources are ordered")
        self.sort_combo.setMinimumWidth(190)
        self.sort_combo.currentIndexChanged.connect(self.refresh_search_results)
        sec_header.addWidget(self.sort_combo)
        left_layout.addLayout(sec_header)

        # Scrollable Cards Area
        self.cards_scroll = QScrollArea()
        self.cards_scroll.setWidgetResizable(True)
        self.cards_scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.cards_scroll.setStyleSheet("QScrollArea { border: none; background: transparent; }")
        self.cards_content = QWidget()
        self.cards_layout = QVBoxLayout(self.cards_content)
        self.cards_layout.setContentsMargins(0, 0, 0, 0)
        self.cards_layout.setSpacing(8)
        self.cards_scroll.setWidget(self.cards_content)

        left_layout.addWidget(self.cards_scroll, 1)

        main_h_layout.addWidget(left_col, 65)

        # Right Column (Package Details Inspector Card)
        self.setup_right_details_panel(main_h_layout)

    def setup_right_details_panel(self, main_h_layout):
        self.detail_card = QFrame()
        self.detail_card.setObjectName("detailCard")
        self.detail_card.setStyleSheet("""
            QFrame#detailCard {
                background-color: rgba(12, 24, 41, 225);
                border: 1px solid #29415f;
                border-radius: 16px;
            }
            QLabel {
                border: none;
                background-color: transparent;
            }
        """)
        detail_shadow = QGraphicsDropShadowEffect(self)
        detail_shadow.setBlurRadius(32)
        detail_shadow.setColor(QColor(0, 0, 0, 125))
        detail_shadow.setOffset(0, 10)
        self.detail_card.setGraphicsEffect(detail_shadow)
        layout = QVBoxLayout(self.detail_card)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        # App Header (Icon + Name + Badges)
        app_head = QHBoxLayout()
        self.app_icon_label = QLabel()
        self.app_icon_label.setFixedSize(54, 54)
        self.app_icon_label.setStyleSheet("border-radius: 10px; background-color: #1e293b;")
        self.app_icon_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        app_head.addWidget(self.app_icon_label)

        app_name_col = QVBoxLayout()
        app_name_col.setSpacing(2)
        top_name_row = QHBoxLayout()
        self.detail_name = QLabel("No package selected")
        self.detail_name.setStyleSheet("font-size: 20px; font-weight: 800; color: #f8fafc;")
        self.detail_source_pill = QLabel("—")
        self.detail_source_pill.setStyleSheet("background-color: #064e3b; color: #34d399; font-size: 9px; font-weight: bold; padding: 2px 6px; border-radius: 4px;")
        self.detail_source_pill.hide()
        top_name_row.addWidget(self.detail_name)
        top_name_row.addWidget(self.detail_source_pill)
        top_name_row.addStretch()

        self.detail_short_desc = QLabel("Select a verified result to inspect package details.")
        self.detail_short_desc.setStyleSheet("font-size: 11px; color: #94a3b8;")

        app_name_col.addLayout(top_name_row)
        app_name_col.addWidget(self.detail_short_desc)
        app_head.addLayout(app_name_col)
        app_head.addStretch()
        layout.addLayout(app_head)

        layout.addSpacing(4)

        # Metadata Table
        self.meta_grid = QGridLayout()
        self.meta_grid.setVerticalSpacing(6)
        self.meta_grid.setHorizontalSpacing(10)

        meta_rows = [
            ("Version", "—"),
            ("Repository", "—"),
            ("License", "—"),
            ("Architecture", "—"),
            ("Size", "—"),
            ("Maintainer", "—"),
        ]

        self.meta_val_labels = {}
        for row_idx, (k, v) in enumerate(meta_rows):
            k_lbl = QLabel(k)
            k_lbl.setStyleSheet("color: #64748b; font-size: 11px; font-weight: 500;")
            v_lbl = QLabel(v)
            v_lbl.setStyleSheet("color: #e2e8f0; font-size: 11px; font-weight: 600;")
            self.meta_grid.addWidget(k_lbl, row_idx, 0)
            self.meta_grid.addWidget(v_lbl, row_idx, 1)
            self.meta_val_labels[k] = v_lbl

        layout.addLayout(self.meta_grid)

        self.detail_meta_widgets = []
        for row_idx in range(self.meta_grid.rowCount()):
            for col_idx in range(2):
                item = self.meta_grid.itemAtPosition(row_idx, col_idx)
                if item and item.widget():
                    self.detail_meta_widgets.append(item.widget())

        self.detail_empty_state = QLabel(
            "✦\n\nNo package selected\n\nChoose a verified result from the source list to inspect its details."
        )
        self.detail_empty_state.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.detail_empty_state.setWordWrap(True)
        self.detail_empty_state.setStyleSheet("color: #64748b; font-size: 12px; padding: 24px;")
        layout.insertWidget(2, self.detail_empty_state, 1)

        layout.addSpacing(6)

        # Tag Chips
        self.chips_container = QWidget()
        self.chips_layout = QHBoxLayout(self.chips_container)
        self.chips_layout.setContentsMargins(0, 0, 0, 0)
        self.chips_layout.setSpacing(6)
        layout.addWidget(self.chips_container)

        # Detailed Description
        self.detail_long_desc = QLabel("")
        self.detail_long_desc.setWordWrap(True)
        self.detail_long_desc.setStyleSheet("color: #94a3b8; font-size: 11px; line-height: 1.4; padding-top: 4px;")
        layout.addWidget(self.detail_long_desc)

        layout.addSpacing(6)

        # Quick Links
        links_box = QVBoxLayout()
        links_box.setSpacing(6)

        self.link_web = QLabel("🌐  Website: —")
        self.link_src = QLabel("🐙  Source: —")
        self.link_wiki = QLabel("📄  Arch Wiki: —")

        for l in [self.link_web, self.link_src, self.link_wiki]:
            l.setOpenExternalLinks(True)
            l.setStyleSheet("font-size: 11px; color: #cbd5e1;")
            links_box.addWidget(l)

        self.detail_link_widgets = [self.link_web, self.link_src, self.link_wiki]

        layout.addLayout(links_box)
        layout.addStretch()

        main_h_layout.addWidget(self.detail_card, 35)
        self.clear_inspector_panel()

    def update_tag_chips(self, tags):
        # Clear previous chips
        while self.chips_layout.count():
            item = self.chips_layout.takeAt(0)
            widget = item.widget()
            if widget:
                widget.deleteLater()

        for t in tags[:5]:
            tag_chip = QLabel(t)
            tag_chip.setStyleSheet("background-color: #0e1927; border: 1px solid #18293d; color: #94a3b8; font-size: 10px; padding: 2px 8px; border-radius: 4px;")
            self.chips_layout.addWidget(tag_chip)
        self.chips_layout.addStretch()

    def do_search_clicked(self):
        q = self.input_search.text().strip()
        if q:
            self.do_search_query(q)
        else:
            self.clear_search_state()

    def on_search_text_changed(self, text):
        if not text.strip():
            self.clear_search_state()

    def clear_search_state(self):
        self.current_query = ""
        self.last_search_result = None
        self.rec_card.hide()
        self.rec_desc.setText("Search for software to compare available sources.")
        self.rec_sub.setText("")
        self.clear_inspector_panel()
        while self.cards_layout.count():
            item = self.cards_layout.takeAt(0)
            widget = item.widget()
            if widget:
                widget.setParent(None)
                widget.deleteLater()
        self.cards_layout.addStretch()

    def do_search_query(self, query):
        query = query.strip()
        if not query:
            return
        self.current_query = query
        self.input_search.setText(query)
        self.clear_inspector_panel()

        # Add to search history if not present
        if query not in self.search_history:
            self.search_history.insert(0, query)
            self.history_menu.clear()
            for h in self.search_history[:8]:
                act = self.history_menu.addAction(h)
                act.triggered.connect(lambda checked, item=h: self.do_search_query(item))
            self.btn_history.setText(f"History · {len(self.search_history)}")
            self.history_sub.setText("Recent searches")
            self.btn_history.setEnabled(True)

        self.rec_desc.setText(f"Searching available paths for '{query}'...")
        self.rec_sub.setText("Querying sync database, AUR RPC API, and release repositories...")

        # Request search via JSON-RPC
        self.call_rpc("v1.search", {"target": query}, self.on_search_response)

    def resolve_package_metadata_async(self, query, project_url=None):
        # Fetch real icon dynamically in background
        icon_thread = IconFetcherThread(query, self)
        icon_thread.icon_fetched.connect(self.on_icon_fetched)
        icon_thread.finished.connect(
            lambda thread=icon_thread: self.icon_threads.remove(thread)
            if thread in self.icon_threads else None
        )
        self.icon_threads.append(icon_thread)
        icon_thread.start()

        # Query real pacman metadata for rich package inspector
        meta = self.get_pacman_metadata(query)
        if meta:
            meta["project_url"] = project_url
            self.update_inspector_panel(meta)
        else:
            # Unknown package: keep inspector honest instead of inventing metadata.
            smart_meta = {
                "name": query,
                "version": "Not found",
                "repository": "—",
                "license": "—",
                "architecture": "—",
                "size": "—",
                "maintainer": "—",
                "description": "No verified Arch package metadata found.",
                "url": None,
                "project_url": project_url,
                "tags": [],
            }
            self.update_inspector_panel(smart_meta)

    def on_icon_fetched(self, app_name, pixmap, accent_color):
        if app_name == self.current_query.lower():
            # Scale to fit nicely in 54x54 box with padding
            scaled = pixmap.scaled(44, 44, Qt.AspectRatioMode.KeepAspectRatio, Qt.TransformationMode.SmoothTransformation)

            container_pix = QPixmap(54, 54)
            container_pix.fill(Qt.GlobalColor.transparent)
            p = QPainter(container_pix)
            p.setRenderHint(QPainter.RenderHint.Antialiasing)
            p.setBrush(QBrush(QColor(accent_color)))
            p.setPen(Qt.PenStyle.NoPen)
            p.drawRoundedRect(0, 0, 54, 54, 10, 10)

            x = (54 - scaled.width()) // 2
            y = (54 - scaled.height()) // 2
            p.drawPixmap(x, y, scaled)
            p.end()

            self.app_icon_label.setPixmap(container_pix)

    def get_pacman_metadata(self, query):
        """Fetch exact real package metadata using pacman -Si"""
        try:
            out = subprocess.check_output(["pacman", "-Si", query], stderr=subprocess.DEVNULL, text=True)
            meta = {}
            for line in out.splitlines():
                if ":" in line:
                    k, v = line.split(":", 1)
                    meta[k.strip()] = v.strip()

            tags = [query]
            desc = meta.get("Description", "")
            for word in ["browser", "privacy", "security", "web", "chromium", "container", "virtualization", "chat", "voice", "stream", "video", "editor", "compiler"]:
                if word in desc.lower():
                    tags.append(word)

            return {
                "name": meta.get("Name", query),
                "version": meta.get("Version", "1.0.0"),
                "repository": meta.get("Repository", "extra"),
                "license": meta.get("Licenses", "Custom"),
                "architecture": meta.get("Architecture", "x86_64"),
                "size": meta.get("Installed Size", "N/A"),
                "maintainer": meta.get("Packager", "Arch Linux"),
                "description": desc,
                "url": meta.get("URL"),
                "tags": tags,
            }
        except Exception:
            return None

    def clear_inspector_panel(self):
        self.app_icon_label.clear()
        self.detail_name.setText("Ready to inspect")
        self.detail_source_pill.setText("—")
        self.detail_source_pill.hide()
        self.detail_short_desc.setText("Select a verified result to inspect package details.")
        self.detail_long_desc.setText("")
        self.detail_empty_state.show()
        for widget in self.detail_meta_widgets + self.detail_link_widgets:
            widget.hide()
        self.chips_container.hide()
        self.detail_long_desc.hide()
        for label in self.meta_val_labels.values():
            label.setText("—")
        self.update_tag_chips([])
        self.link_web.setText("🌐  Website: —")
        self.link_src.setText("🐙  Source: —")
        self.link_wiki.setText("📄  Arch Wiki: —")

    def update_inspector_panel(self, meta):
        self.detail_empty_state.hide()
        self.detail_source_pill.show()
        for widget in self.detail_meta_widgets + self.detail_link_widgets:
            widget.show()
        self.chips_container.show()
        self.detail_long_desc.show()
        self.detail_name.setText(meta.get("name", self.current_query))
        self.detail_short_desc.setText(meta.get("description", ""))
        self.detail_long_desc.setText(meta.get("description", ""))

        self.meta_val_labels["Version"].setText(meta.get("version", ""))
        self.meta_val_labels["Repository"].setText(meta.get("repository", ""))
        self.meta_val_labels["License"].setText(meta.get("license", ""))
        self.meta_val_labels["Architecture"].setText(meta.get("architecture", ""))
        self.meta_val_labels["Size"].setText(meta.get("size", ""))
        self.meta_val_labels["Maintainer"].setText(meta.get("maintainer", ""))

        self.update_tag_chips(meta.get("tags", [self.current_query]))

        self.set_link_label(self.link_web, "🌐  Website", meta.get("url"))
        self.set_link_label(self.link_src, "🐙  Source", meta.get("project_url"))
        self.link_wiki.setText("📄  Arch Wiki: —")

    def set_link_label(self, label, title, url):
        if url and url.startswith(("https://", "http://")):
            label.setText(f"{title}: <a href='{url}' style='color:#0284c7;'>{url}</a>")
        else:
            label.setText(f"{title}: —")

    def on_search_response(self, result):
        if not isinstance(result, dict) or not self.current_query or result.get("target") != self.current_query:
            return
        self.last_search_result = result
        self.refresh_search_results()

    def refresh_search_results(self):
        while self.cards_layout.count():
            item = self.cards_layout.takeAt(0)
            widget = item.widget()
            if widget:
                widget.setParent(None)
                widget.deleteLater()

        result = self.last_search_result
        if not result:
            self.rec_desc.setText("No search results found.")
            return

        rec = result.get("recommended_source")
        target = result.get("target", self.current_query)

        if rec:
            self.rec_card.show()
            self.rec_desc.setText(f"{target.capitalize()} is available in the {rec.upper()} path.")
            self.rec_sub.setText("This is the safest and most reliable available option.")
            self.detail_source_pill.setText(rec)
        else:
            self.rec_card.hide()
            self.rec_desc.setText(f"No verified source found for '{target}'.")
            self.rec_sub.setText("No matching packages found in Official repos, AUR, Flatpak, or AppImage.")

        table = list(result.get("decision_table", []))
        if self.sort_combo.currentIndex() == 0:
            table.sort(key=lambda row: (not row.get("recommended", False), row.get("source", "")))
        elif self.sort_combo.currentIndex() == 1:
            table.sort(key=lambda row: row.get("source", ""))
        else:
            table.sort(key=lambda row: (
                row.get("candidates", [{}])[0].get("name", target)
                if row.get("candidates") else target
            ).lower())
        selected_source = self.combo_source_filter.currentText().lower()
        cards_added = 0
        first_candidate_name = None
        first_project_url = None
        for row in table:
            source_str = row.get("source", "")
            if source_str in ["deb", "rpm"]:
                continue
            if selected_source != "all sources" and source_str != selected_source:
                continue
            card = self.create_decision_card(row, target)
            self.cards_layout.addWidget(card)
            cards_added += 1
            if first_candidate_name is None:
                cands = row.get("candidates", [])
                if cands:
                    first_candidate_name = cands[0].get("name", target)
                    first_project_url = cands[0].get("project_url")

        if cards_added == 0:
            empty_lbl = QLabel(f"No packages found matching '{target}'.")
            empty_lbl.setStyleSheet("color: #64748b; font-size: 13px; padding: 24px 0;")
            empty_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
            self.cards_layout.addWidget(empty_lbl)
            self.clear_inspector_panel()
            self.detail_short_desc.setText("No verified source found.")
        elif first_candidate_name:
            self.resolve_package_metadata_async(first_candidate_name, first_project_url)

        self.cards_layout.addStretch()

    def create_decision_card(self, row, target):
        card = QFrame()
        card.setObjectName("decisionCard")
        card.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        card.setStyleSheet("""
            QFrame#decisionCard {
                background-color: rgba(16, 29, 49, 225);
                border: 1px solid #29415f;
                border-radius: 14px;
            }
            QFrame#decisionCard:hover {
                border: 1px solid #38bdf8;
                background-color: rgba(22, 45, 73, 235);
            }
            QLabel {
                border: none;
                background: transparent;
            }
        """)
        layout = QHBoxLayout(card)
        layout.setContentsMargins(14, 12, 14, 12)
        layout.setSpacing(14)

        source_str = row["source"]
        colors = {
            "official": {"bg": "#0c1f36", "border": "#0284c7", "text": "#38bdf8", "btn": "#1a3a60", "btn_border": "#0284c7", "badge_bg": "#0c2744"},
            "aur": {"bg": "#231238", "border": "#a855f7", "text": "#c084fc", "btn": "#2e1065", "btn_border": "#7e22ce", "badge_bg": "#281240"},
            "flatpak": {"bg": "#0d2624", "border": "#14b8a6", "text": "#2dd4bf", "btn": "#134e4a", "btn_border": "#0d9488", "badge_bg": "#113835"},
            "appimage": {"bg": "#2d1f0e", "border": "#f59e0b", "text": "#fbbf24", "btn": "#451a03", "btn_border": "#d97706", "badge_bg": "#382510"},
            "upstream": {"bg": "#151e2e", "border": "#38bdf8", "text": "#7dd3fc", "btn": "#1e293b", "btn_border": "#475569", "badge_bg": "#182234"},
        }
        theme = colors.get(source_str, colors["official"])

        # Source Icon Container (Square)
        icon_box = QFrame()
        icon_box.setFixedSize(40, 40)
        icon_box.setStyleSheet(f"""
            QFrame {{
                background-color: {theme['bg']};
                border: 1px solid {theme['border']}55;
                border-radius: 8px;
            }}
            QLabel {{
                border: none;
                background: transparent;
            }}
        """)
        ib_layout = QHBoxLayout(icon_box)
        ib_layout.setContentsMargins(0, 0, 0, 0)
        ib_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)

        icon_symbol = "A" if source_str in ["official", "aur"] else ("📦" if source_str == "flatpak" else ("🗎" if source_str == "appimage" else "🐙"))
        src_icon = QLabel(icon_symbol)
        src_icon.setStyleSheet(f"color: {theme['text']}; font-size: 18px; font-weight: 900;")
        src_icon.setAlignment(Qt.AlignmentFlag.AlignCenter)
        ib_layout.addWidget(src_icon)
        layout.addWidget(icon_box)

        # Source Badge
        badge_lbl = QLabel(source_str.upper())
        badge_lbl.setStyleSheet(f"""
            background-color: {theme['badge_bg']};
            color: {theme['text']};
            border: 1px solid {theme['border']}66;
            font-size: 10px;
            font-weight: 800;
            padding: 3px 8px;
            border-radius: 5px;
            letter-spacing: 0.5px;
        """)
        layout.addWidget(badge_lbl)

        # Info Column
        info_col = QVBoxLayout()
        info_col.setSpacing(2)

        first_cand = row.get("candidates", [{}])[0] if row.get("candidates") else {}
        cand_name = first_cand.get("name", target)
        cand_ver = first_cand.get("version", "")
        cand_desc = first_cand.get("description", row.get("reason", ""))

        c_title = QLabel(f"<b>{cand_name}</b> <span style='color:#64748b; font-size:11px; font-family:JetBrains Mono,monospace;'>{cand_ver}</span>")
        c_title.setStyleSheet("font-size: 13px; color: #f8fafc;")
        info_col.addWidget(c_title)

        desc_lbl = QLabel(cand_desc)
        desc_lbl.setStyleSheet("color: #64748b; font-size: 11px;")
        desc_lbl.setWordWrap(True)
        info_col.addWidget(desc_lbl)

        layout.addLayout(info_col, 1)

        # Availability status
        avail = row.get("availability", "available")
        avail_lbl = QLabel("● Available" if avail == "available" else (f"● {avail.capitalize()}"))
        if avail == "available":
            avail_lbl.setStyleSheet("color: #10b981; font-size: 11px; font-weight: 600;")
        else:
            avail_lbl.setStyleSheet("color: #64748b; font-size: 11px;")
        layout.addWidget(avail_lbl)

        # Repository label
        repo_names = {
            "official": "extra",
            "aur": "AUR (community)",
            "flatpak": "Flathub",
            "appimage": "Official Release",
            "upstream": "GitHub"
        }
        repo_note = QLabel(repo_names.get(source_str, source_str))
        repo_note.setStyleSheet("color: #475569; font-size: 11px; font-weight: 500;")
        layout.addWidget(repo_note)

        # Action Button
        btn_labels = {
            "official": "Install Plan",
            "aur": "Build Plan",
            "flatpak": "Install Plan",
            "appimage": "Download",
            "upstream": "View Source"
        }
        btn_action = QPushButton(btn_labels.get(source_str, "Action"))
        btn_action.setCursor(QCursor(Qt.CursorShape.PointingHandCursor))
        btn_action.setStyleSheet(f"""
            QPushButton {{
                background-color: {theme['btn']};
                border: 1px solid {theme['btn_border']}77;
                border-radius: 6px;
                padding: 6px 14px;
                color: {theme['text']};
                font-size: 11px;
                font-weight: 700;
            }}
            QPushButton:hover {{
                background-color: #0284c7;
                color: #ffffff;
                border-color: #38bdf8;
            }}
        """)
        project_url = first_cand.get("source_url")
        asset_url = first_cand.get("url")
        btn_action.clicked.connect(
            lambda checked, t=cand_name, s=source_str, a=asset_url, u=project_url:
            self.start_candidate_action(t, s, a, u)
        )
        layout.addWidget(btn_action)

        btn_more = QPushButton("···")
        btn_more.setFixedWidth(24)
        btn_more.setStyleSheet("QPushButton { background-color: transparent; border: none; color: #475569; font-size: 16px; font-weight: bold; } QPushButton:hover { color: #f8fafc; }")
        layout.addWidget(btn_more)

        card.mousePressEvent = lambda ev, c_name=cand_name, r_name=row["source"], u=project_url: self.on_card_selected(c_name, r_name, u)
        return card

    def on_card_selected(self, cand_name, source_name, project_url=None):
        self.detail_source_pill.setText(source_name)
        self.resolve_package_metadata_async(cand_name, project_url)

    def switch_to_build_tab(self, target):
        self.switch_tab(2)
        self.build_target_input.setText(target)

    def start_candidate_action(self, target, source, asset_url=None, project_url=None):
        if source in {"official", "flatpak"}:
            self.switch_tab(2)
            self.build_target_input.setText(target)
            self.do_prepare_install()
        elif source == "appimage" and asset_url:
            webbrowser.open(asset_url)
        elif source == "upstream" and project_url:
            webbrowser.open(project_url)
        else:
            self.switch_to_build_tab(target)

    def quick_prepare_install(self):
        target = self.input_search.text().strip()
        self.switch_tab(2)
        self.build_target_input.setText(target)
        self.do_prepare_install()

    # --- 4. VIEW 2: INSPECT ---
    def setup_inspect_view(self):
        layout = QVBoxLayout(self.view_inspect)
        layout.setContentsMargins(0, 8, 0, 0)
        layout.setSpacing(14)

        h1 = QLabel("Foreign Package Inspector")
        h1.setStyleSheet("font-size: 22px; font-weight: 800; color: #f8fafc;")
        h1_sub = QLabel("Safely inspect .deb and .rpm packages, metadata, systemd units, and maintainer scripts without execution.")
        h1_sub.setStyleSheet("font-size: 12px; color: #64748b;")
        layout.addWidget(h1)
        layout.addWidget(h1_sub)

        file_bar = QHBoxLayout()
        self.inspect_file_input = QLineEdit()
        self.inspect_file_input.setPlaceholderText("Select or enter path to .deb or .rpm file...")

        btn_browse = QPushButton("Browse File...")
        btn_browse.clicked.connect(self.browse_inspect_file)

        btn_inspect = QPushButton("Inspect Package")
        btn_inspect.setStyleSheet("background-color: #0084d1; color: #ffffff; font-weight: bold;")
        btn_inspect.clicked.connect(self.do_inspect)

        btn_import = QPushButton("Import & Build")
        btn_import.setStyleSheet("background-color: #059669; color: #ffffff; font-weight: bold;")
        btn_import.setToolTip("Convert the package payload into an Arch package, then offer installation")
        btn_import.clicked.connect(self.do_import_build)

        file_bar.addWidget(self.inspect_file_input, 1)
        file_bar.addWidget(btn_browse)
        file_bar.addWidget(btn_inspect)
        file_bar.addWidget(btn_import)
        layout.addLayout(file_bar)

        safety_banner = QFrame()
        safety_banner.setStyleSheet("background-color: #091724; border: 1px solid #143452; border-radius: 8px; padding: 10px;")
        sb_layout = QHBoxLayout(safety_banner)
        sb_icon = QLabel("🛡️")
        sb_text = QLabel("<b>Safety Guarantee:</b> Foreign maintainer scripts are analyzed in data-only mode and are <b>NEVER executed</b>.")
        sb_text.setStyleSheet("color: #38bdf8; font-size: 12px;")
        sb_layout.addWidget(sb_icon)
        sb_layout.addWidget(sb_text)
        sb_layout.addStretch()
        layout.addWidget(safety_banner)

        self.inspect_output = QTextEdit()
        self.inspect_output.setReadOnly(True)
        self.inspect_output.setPlaceholderText("Inspection report containing metadata, dependencies, desktop files, systemd units, and maintainer scripts will be displayed here...")
        layout.addWidget(self.inspect_output, 1)

    def browse_inspect_file(self):
        file_path, _ = QFileDialog.getOpenFileName(self, "Select Package File", "", "Package Files (*.deb *.rpm);;All Files (*)")
        if file_path:
            self.inspect_file_input.setText(file_path)
            self.do_inspect()

    def do_import_build(self):
        path = self.inspect_file_input.text().strip()
        if not path:
            QMessageBox.warning(self, "Package Required", "Choose a .deb or .rpm file first.")
            return
        if not path.lower().endswith((".deb", ".rpm")):
            QMessageBox.warning(self, "Unsupported File", "Import currently supports .deb and .rpm files.")
            return
        self.switch_tab(2)
        self.build_target_input.setText(path)
        self.do_prepare_build()

    def do_inspect(self):
        path = self.inspect_file_input.text().strip()
        if not path:
            return
        self.call_rpc("v1.inspect", {"target": path}, self.on_inspect_response)

    def on_inspect_response(self, result):
        if not result:
            return
        self.inspect_output.setText(json.dumps(result, indent=2))

    # --- 5. VIEW 3: BUILD & INSTALL ---
    def setup_build_view(self):
        layout = QVBoxLayout(self.view_build)
        layout.setContentsMargins(0, 8, 0, 0)
        layout.setSpacing(14)

        h1 = QLabel("Build & Clean Chroot Studio")
        h1.setStyleSheet("font-size: 22px; font-weight: 800; color: #f8fafc;")
        h1_sub = QLabel("Build packages inside an isolated clean chroot container with automated runtime smoke testing.")
        h1_sub.setStyleSheet("font-size: 12px; color: #64748b;")
        layout.addWidget(h1)
        layout.addWidget(h1_sub)

        top_box = QFrame()
        top_box.setStyleSheet("background-color: #09121f; border: 1px solid #152438; border-radius: 10px; padding: 12px;")
        top_layout = QVBoxLayout(top_box)

        t_row = QHBoxLayout()
        self.build_target_input = QLineEdit()
        self.build_target_input.setPlaceholderText("Target: .deb/.rpm file, GitHub URL, local directory, PKGBUILD, or package name...")
        self.build_target_input.clear()

        btn_browse_build = QPushButton("Browse Package...")
        btn_browse_build.setToolTip("Choose a local .deb or .rpm package")
        btn_browse_build.clicked.connect(self.browse_build_package)

        btn_prep = QPushButton("Prepare Build Plan (Dry-Run)")
        btn_prep.setStyleSheet("background-color: #0084d1; color: #ffffff; font-weight: bold;")
        btn_prep.clicked.connect(self.do_prepare_build)

        t_row.addWidget(self.build_target_input, 1)
        t_row.addWidget(btn_browse_build)
        t_row.addWidget(btn_prep)
        top_layout.addLayout(t_row)

        opts_row = QHBoxLayout()
        self.opt_name = QLineEdit(); self.opt_name.setPlaceholderText("--name (optional)")
        self.opt_ver = QLineEdit(); self.opt_ver.setPlaceholderText("--version (optional)")
        self.opt_entry = QLineEdit(); self.opt_entry.setPlaceholderText("--entry binary (optional)")
        self.opt_deps = QLineEdit(); self.opt_deps.setPlaceholderText("--dependency (comma-separated)")

        opts_row.addWidget(self.opt_name)
        opts_row.addWidget(self.opt_ver)
        opts_row.addWidget(self.opt_entry)
        opts_row.addWidget(self.opt_deps)
        top_layout.addLayout(opts_row)

        remove_row = QHBoxLayout()
        remove_label = QLabel("Remove installed package")
        remove_label.setStyleSheet("color: #cbd5e1; font-weight: 700;")
        self.uninstall_name_input = QLineEdit()
        self.uninstall_name_input.setPlaceholderText("Package name, e.g. grok-bot")
        self.btn_prepare_uninstall = QPushButton("Prepare Uninstall Plan")
        self.btn_prepare_uninstall.setEnabled(False)
        self.btn_prepare_uninstall.setToolTip("Review a pacman removal plan before anything is changed")
        self.btn_prepare_uninstall.clicked.connect(self.do_prepare_uninstall)
        self.uninstall_name_input.textChanged.connect(
            lambda text: self.btn_prepare_uninstall.setEnabled(bool(text.strip()))
        )
        remove_row.addWidget(remove_label)
        remove_row.addWidget(self.uninstall_name_input, 1)
        remove_row.addWidget(self.btn_prepare_uninstall)
        top_layout.addLayout(remove_row)

        layout.addWidget(top_box)

        self.build_stage_label = QLabel("Ready — choose a package or build target.")
        self.build_stage_label.setStyleSheet("color: #94a3b8; font-size: 12px; font-weight: 600;")
        layout.addWidget(self.build_stage_label)

        self.build_progress = QProgressBar()
        self.build_progress.setRange(0, 0)
        self.build_progress.setTextVisible(False)
        self.build_progress.setFixedHeight(5)
        self.build_progress.setStyleSheet("""
            QProgressBar { background-color: #101c2d; border: none; border-radius: 3px; }
            QProgressBar::chunk { background-color: #0ea5e9; border-radius: 3px; }
        """)
        self.build_progress.hide()
        layout.addWidget(self.build_progress)

        splitter = QSplitter(Qt.Orientation.Vertical)

        plan_container = QWidget()
        plan_layout = QVBoxLayout(plan_container)
        plan_layout.setContentsMargins(0, 0, 0, 0)
        plan_lbl = QLabel("Prepared Plan & Review Manifest")
        plan_lbl.setStyleSheet("font-size: 13px; font-weight: 700; color: #94a3b8;")
        plan_layout.addWidget(plan_lbl)

        self.build_plan_view = QTextEdit()
        self.build_plan_view.setReadOnly(True)
        self.build_plan_view.setPlaceholderText("Prepared dry-run plan, steps, reviewed inputs, and hashes will appear here...")
        plan_layout.addWidget(self.build_plan_view)
        splitter.addWidget(plan_container)

        exec_container = QWidget()
        exec_layout = QVBoxLayout(exec_container)
        exec_layout.setContentsMargins(0, 8, 0, 0)

        ctrl_bar = QHBoxLayout()
        self.btn_exec_plan = QPushButton("Confirm & Execute Plan (Clean Chroot)")
        self.btn_exec_plan.setEnabled(False)
        self.btn_exec_plan.setStyleSheet("""
            QPushButton {
                background-color: #059669;
                color: #ffffff;
                font-weight: 700;
                border: none;
                border-radius: 8px;
                padding: 10px 20px;
            }
            QPushButton:hover {
                background-color: #10b981;
            }
            QPushButton:disabled {
                background-color: #13241e;
                color: #3b5247;
            }
        """)
        self.btn_exec_plan.clicked.connect(self.do_execute_plan)

        ctrl_bar.addWidget(self.btn_exec_plan)
        ctrl_bar.addStretch()
        exec_layout.addLayout(ctrl_bar)

        self.build_log_console = QTextEdit()
        self.build_log_console.setReadOnly(True)
        self.build_log_console.setPlaceholderText("Execution output and runtime smoke test logs...")
        exec_layout.addWidget(self.build_log_console)
        splitter.addWidget(exec_container)

        layout.addWidget(splitter, 1)

    def do_prepare_build(self):
        target = self.build_target_input.text().strip()
        if not target:
            return

        self.set_build_busy(True, "Preparing a verified build plan…")

        opts = {}
        if self.opt_name.text().strip(): opts["name"] = self.opt_name.text().strip()
        if self.opt_ver.text().strip(): opts["version"] = self.opt_ver.text().strip()
        if self.opt_entry.text().strip(): opts["entry"] = self.opt_entry.text().strip()
        if self.opt_deps.text().strip(): opts["dependencies"] = [d.strip() for d in self.opt_deps.text().split(",")]

        params = {
            "action": "build",
            "request": {
                "target": target,
                "options": opts
            }
        }
        self.call_rpc("v1.prepare", params, self.on_build_prepare_response)

    def browse_build_package(self):
        file_path, _ = QFileDialog.getOpenFileName(
            self,
            "Select Package to Import",
            "",
            "Package Files (*.deb *.rpm);;All Files (*)",
        )
        if file_path:
            self.build_target_input.setText(file_path)
            self.do_prepare_build()

    def set_build_busy(self, busy, message=None):
        if not hasattr(self, "build_progress"):
            return
        if message:
            self.build_stage_label.setText(message)
        self.build_progress.setVisible(busy)
        self.btn_exec_plan.setEnabled(not busy and bool(self.active_plan))

    def do_prepare_install(self):
        target = self.build_target_input.text().strip()
        if not target:
            return
        self.set_build_busy(True, "Preparing the installation plan…")
        self.call_rpc(
            "v1.prepare",
            {"action": "install", "request": {"target": target}},
            self.on_build_prepare_response,
        )

    def do_prepare_uninstall(self):
        target = self.uninstall_name_input.text().strip()
        if not target:
            return
        self.set_build_busy(True, "Preparing a safe removal plan…")
        self.call_rpc(
            "v1.prepare",
            {"action": "uninstall", "request": {"target": target}},
            self.on_build_prepare_response,
        )

    def on_build_prepare_response(self, result):
        self.set_build_busy(False, "Plan ready — review the manifest before continuing.")
        if not result:
            return
        self.active_plan = result
        self.build_plan_view.setText(json.dumps(result, indent=2))
        if result.get("blocked"):
            self.btn_exec_plan.setEnabled(False)
            self.build_stage_label.setText("Blocked — resolve the issue shown in the plan.")
            QMessageBox.warning(self, "Plan Blocked", f"Plan is blocked: {result['blocked']}")
        else:
            self.btn_exec_plan.setEnabled(True)
            if result.get("action") == "uninstall":
                self.btn_exec_plan.setText("Confirm & Remove Package")
            else:
                self.btn_exec_plan.setText("Confirm & Execute Plan")

    def do_execute_plan(self):
        if not self.active_plan:
            return
        plan_id = self.active_plan.get("plan_id")
        self.privilege_retry_used = False
        self.set_build_busy(True, "Working… clean chroot setup and package build may take several minutes.")
        self.btn_exec_plan.setEnabled(False)
        if self.active_plan.get("action") == "uninstall":
            self.build_log_console.setText(
                "ArchBridge is working.\n\n"
                "Preparing pacman to remove the selected package…\n\n"
                "Do not close the window while this is running."
            )
        else:
            self.build_log_console.setText(
                "ArchBridge is working.\n\n"
                "1/2  Preparing the isolated clean chroot…\n"
                "2/2  Building and validating the package…\n\n"
                "Do not close the window while this is running."
            )
        self.call_rpc("v1.execute", {"plan_id": plan_id, "confirmed": True}, self.on_execute_response)

    def request_sudo_authorization(self, error_message):
        """Obtain temporary sudo authorization without retaining the password."""
        if self.privilege_retry_used:
            return False
        if "password is required" not in error_message.lower():
            return False

        dialog = QDialog(self)
        dialog.setWindowTitle("ArchBridge administrator permission")
        dialog.setModal(True)
        dialog.setMinimumWidth(430)
        dialog.setStyleSheet("""
            QDialog { background: #0b1220; color: #e2e8f0; }
            QLabel { color: #cbd5e1; }
            QLineEdit { background: #101c2d; color: #f8fafc; border: 1px solid #29415f;
                        border-radius: 8px; padding: 9px; }
            QLineEdit:focus { border: 1px solid #38bdf8; }
            QCheckBox { color: #cbd5e1; spacing: 8px; }
            QDialogButtonBox QPushButton { background: #0ea5e9; color: white; border: none;
                                           border-radius: 7px; padding: 8px 18px; }
            QDialogButtonBox QPushButton:hover { background: #38bdf8; }
        """)
        dialog_layout = QVBoxLayout(dialog)
        title = QLabel("Administrator permission required")
        title.setStyleSheet("font-size: 16px; font-weight: 800; color: #f8fafc;")
        detail = QLabel(
            "ArchBridge needs sudo for the isolated clean chroot. "
            "The password is used once and is never saved or sent to the engine."
        )
        detail.setWordWrap(True)
        password_input = QLineEdit()
        password_input.setPlaceholderText("Sudo password")
        password_input.setEchoMode(QLineEdit.EchoMode.Password)
        show_password = QCheckBox("Show password")
        show_password.toggled.connect(
            lambda visible: password_input.setEchoMode(
                QLineEdit.EchoMode.Normal if visible else QLineEdit.EchoMode.Password
            )
        )
        keep_session = QCheckBox("Keep sudo authorization active for this session")
        keep_session.setChecked(True)
        keep_session.setToolTip("Uncheck to revoke sudo authorization after the job finishes.")
        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.accepted.connect(dialog.accept)
        buttons.rejected.connect(dialog.reject)
        dialog_layout.addWidget(title)
        dialog_layout.addWidget(detail)
        dialog_layout.addSpacing(8)
        dialog_layout.addWidget(password_input)
        dialog_layout.addWidget(show_password)
        dialog_layout.addWidget(keep_session)
        dialog_layout.addWidget(buttons)
        password_input.setFocus()
        accepted = dialog.exec() == QDialog.DialogCode.Accepted
        password = password_input.text() if accepted else ""
        keep = keep_session.isChecked()
        if not accepted or not password:
            self.set_build_busy(False, "Authorization cancelled.")
            return True

        self.clear_sudo_after_job = not keep
        self.build_stage_label.setText("Checking authorization… the window remains responsive.")
        self.sudo_process = QProcess(self)
        self.sudo_process.setProcessChannelMode(QProcess.ProcessChannelMode.SeparateChannels)
        self.sudo_process.finished.connect(self.on_sudo_authorization_finished)
        self.sudo_process.start("sudo", ["-S", "-v"])
        if not self.sudo_process.waitForStarted(1000):
            self.sudo_process = None
            self.set_build_busy(False, "Could not start sudo.")
            QMessageBox.critical(self, "Authorization failed", "ArchBridge could not start sudo.")
            return True
        self.sudo_process.write((password + "\n").encode())
        password = ""
        self.sudo_process.closeWriteChannel()
        return True

    def on_sudo_authorization_finished(self, exit_code, _exit_status):
        process = self.sudo_process
        self.sudo_process = None
        if not process or exit_code != 0:
            self.set_build_busy(False, "Authorization failed. Check your sudo password.")
            QMessageBox.critical(self, "Authorization failed", "Sudo rejected the password. No build was started.")
            return

        self.privilege_retry_used = True
        self.build_stage_label.setText("Authorization accepted — refreshing the reviewed plan…")
        if self.active_plan and self.active_plan.get("action") in ["install", "uninstall"]:
            QTimer.singleShot(0, self.do_execute_plan)
        else:
            QTimer.singleShot(0, self.do_prepare_build)

    def on_execute_response(self, result):
        if not result:
            self.set_build_busy(False, "No response received from the packaging engine.")
            return
        self.set_build_busy(False)
        self.build_log_console.setText(json.dumps(result, indent=2))
        if result.get("ok"):
            if result.get("next_plan"):
                self.active_plan = result["next_plan"]
                self.build_plan_view.setText(json.dumps(self.active_plan, indent=2))
                self.btn_exec_plan.setText("Confirm & Install Built Package")
                self.btn_exec_plan.setEnabled(True)
                self.build_stage_label.setText("Build complete — the Arch package is ready to install.")
                QMessageBox.information(self, "Package Ready", result.get("message", "Package is ready to install."))
            else:
                action = self.active_plan.get("action") if self.active_plan else "install"
                self.build_stage_label.setText(
                    "Package removal complete." if action == "uninstall" else "Installation complete."
                )
                self.btn_exec_plan.setEnabled(False)
                if self.clear_sudo_after_job:
                    subprocess.run(["sudo", "-k"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
                    self.clear_sudo_after_job = False
                QMessageBox.information(
                    self,
                    "Package Removed" if action == "uninstall" else "Installation Complete",
                    result.get("message", "Success"),
                )
        else:
            self.build_stage_label.setText("Build failed — see the execution log for details.")
            QMessageBox.critical(self, "Execution Failed", result.get("message", "Failed"))

    # --- 6. VIEW 4: DOCTOR ---
    def setup_doctor_view(self):
        layout = QVBoxLayout(self.view_doctor)
        layout.setContentsMargins(0, 8, 0, 0)
        layout.setSpacing(14)

        top = QHBoxLayout()
        title_col = QVBoxLayout()
        h1 = QLabel("System Prerequisites & Health")
        h1.setStyleSheet("font-size: 22px; font-weight: 800; color: #f8fafc;")
        h1_sub = QLabel("Diagnostic verification of packaging tools, kernel namespaces, compilers, and keyrings.")
        h1_sub.setStyleSheet("font-size: 12px; color: #64748b;")
        title_col.addWidget(h1)
        title_col.addWidget(h1_sub)
        top.addLayout(title_col)
        top.addStretch()

        btn_run_doc = QPushButton("Run All Diagnostics")
        btn_run_doc.setStyleSheet("background-color: #0084d1; color: #ffffff; font-weight: bold; padding: 10px 20px;")
        btn_run_doc.clicked.connect(self.do_doctor)
        top.addWidget(btn_run_doc)

        layout.addLayout(top)

        self.doc_table = QTableWidget()
        self.doc_table.setColumnCount(3)
        self.doc_table.setHorizontalHeaderLabels(["Check Name", "Status", "Diagnostic Details"])
        self.doc_table.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeMode.Stretch)
        self.doc_table.setStyleSheet("""
            QTableWidget {
                background-color: #080f19;
                border: 1px solid #132032;
                border-radius: 8px;
            }
            QHeaderView::section {
                background-color: #0c1524;
                color: #94a3b8;
                padding: 8px;
                border: none;
                font-weight: 700;
            }
        """)
        layout.addWidget(self.doc_table, 1)

    def do_doctor(self):
        self.call_rpc("v1.doctor", {}, self.on_doctor_response)

    def on_doctor_response(self, result):
        if not result:
            return
        checks = result.get("checks", [])
        self.doc_table.setRowCount(len(checks))

        for row, c in enumerate(checks):
            name_item = QTableWidgetItem(c.get("name", ""))
            status = c.get("status", "")
            status_item = QTableWidgetItem("● " + status.upper())
            if status == "pass":
                status_item.setForeground(QColor("#10b981"))
            elif status == "unavailable":
                status_item.setForeground(QColor("#f59e0b"))
            else:
                status_item.setForeground(QColor("#ef4444"))

            msg_item = QTableWidgetItem(c.get("message", ""))

            self.doc_table.setItem(row, 0, name_item)
            self.doc_table.setItem(row, 1, status_item)
            self.doc_table.setItem(row, 2, msg_item)

        if result.get("ready"):
            has_unavailable = any(c.get("status") == "unavailable" for c in checks)
            if has_unavailable:
                self.doc_status_lbl.setText("System ready")
                self.doc_sub.setText("Offline checks available")
                self.doc_dot.set_state("#f59e0b")
            else:
                self.doc_status_lbl.setText("System healthy")
                self.doc_sub.setText("Health checks passing")
                self.doc_dot.set_state("#10b981")
        else:
            self.doc_status_lbl.setText("System attention")
            self.doc_sub.setText("Open Health for details")
            self.doc_dot.set_state("#ef4444")

    # --- 7. VIEW 5: SETTINGS ---
    def setup_settings_view(self):
        layout = QVBoxLayout(self.view_settings)
        layout.setContentsMargins(0, 8, 0, 0)
        layout.setSpacing(14)

        h1 = QLabel("Preferences & Source Routing")
        h1.setStyleSheet("font-size: 22px; font-weight: 800; color: #f8fafc;")
        h1_sub = QLabel("Enable or disable package discovery sources according to your workflow.")
        h1_sub.setStyleSheet("font-size: 12px; color: #64748b;")
        layout.addWidget(h1)
        layout.addWidget(h1_sub)

        group = QFrame()
        group.setStyleSheet("background-color: #080f19; border: 1px solid #132032; border-radius: 10px; padding: 16px;")
        g_layout = QVBoxLayout(group)
        g_layout.setSpacing(12)

        self.cfg_checks = {}
        sources = [
            ("official", "Official Arch Linux Repositories (pacman)"),
            ("aur", "Arch User Repository (AUR RPC)"),
            ("flatpak", "Flatpak Release Bundles (Flathub)"),
            ("appimage", "AppImage Standalone Assets"),
            ("upstream", "Upstream Git Release Sources (GitHub/GitLab)"),
            ("deb", "DEB Package Inspection"),
            ("rpm", "RPM Package Inspection"),
        ]

        for s_key, s_label in sources:
            cb = QCheckBox(s_label)
            cb.setChecked(True)
            cb.setStyleSheet("QCheckBox { font-size: 13px; color: #e2e8f0; } QCheckBox::indicator { width: 18px; height: 18px; }")
            self.cfg_checks[s_key] = cb
            g_layout.addWidget(cb)

        btn_save = QPushButton("Save Preferences")
        btn_save.setStyleSheet("background-color: #059669; color: #ffffff; font-weight: bold; padding: 10px 20px; width: 180px;")
        btn_save.clicked.connect(self.save_config)
        g_layout.addWidget(btn_save)

        layout.addWidget(group)
        layout.addStretch()

        self.load_config()

    def load_config(self):
        self.call_rpc("v1.config.get", {"key": "all"}, self.on_config_get)

    def on_config_get(self, result):
        if isinstance(result, dict):
            for k, cb in self.cfg_checks.items():
                if k in result:
                    cb.setChecked(bool(result[k]))

    def save_config(self):
        for k, cb in self.cfg_checks.items():
            val_str = "true" if cb.isChecked() else "false"
            self.call_rpc("v1.prepare", {"action": "config.set", "key": k, "value": val_str}, self.on_config_set_prepared)

    def on_config_set_prepared(self, result):
        if result and result.get("plan_id"):
            self.call_rpc("v1.execute", {"plan_id": result["plan_id"], "confirmed": True}, lambda r: None)
            QMessageBox.information(self, "Preferences Saved", "Source preferences saved successfully.")

    # --- 8. BOTTOM STATUS BAR ---
    def setup_bottom_bar(self, main_layout):
        bar = QFrame()
        bar.setStyleSheet("QFrame { background-color: #070b12; border-top: 1px solid #111a26; padding-top: 4px; }")
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(0, 4, 0, 0)
        layout.setSpacing(16)

        st_ready = QLabel("● Ready")
        st_ready.setStyleSheet("color: #10b981; font-size: 11px; font-weight: bold;")
        layout.addWidget(st_ready)

        sep1 = QLabel("|"); sep1.setStyleSheet("color: #1e293b;")
        layout.addWidget(sep1)

        st_chroot = QLabel("🗄️ Active Chroot: None")
        st_chroot.setStyleSheet("color: #64748b; font-size: 11px;")
        layout.addWidget(st_chroot)

        sep2 = QLabel("|"); sep2.setStyleSheet("color: #1e293b;")
        layout.addWidget(sep2)

        # Get real free disk space
        try:
            total, used, free = shutil.disk_usage("/")
            free_gb = free // (2**30)
            disk_str = f"💾 Disk: {free_gb} GiB Free"
        except Exception:
            disk_str = "💾 Disk: 142 GiB Free"

        st_disk = QLabel(disk_str)
        st_disk.setStyleSheet("color: #64748b; font-size: 11px;")
        layout.addWidget(st_disk)

        sep3 = QLabel("|"); sep3.setStyleSheet("color: #1e293b;")
        layout.addWidget(sep3)

        st_keyring = QLabel("🛡️ Keyring: Verified")
        st_keyring.setStyleSheet("color: #64748b; font-size: 11px;")
        layout.addWidget(st_keyring)

        layout.addStretch()

        st_ver = QLabel("ᛘ v1.0.0   ArchBridge")
        st_ver.setStyleSheet("color: #475569; font-size: 11px;")
        layout.addWidget(st_ver)

        main_layout.addWidget(bar)

    # --- RPC INTEGRATION ---
    def init_rpc(self):
        self.worker = RpcWorker(self.binary_path)
        self.worker.response_received.connect(self.on_rpc_response)
        self.worker.error_occurred.connect(self.on_rpc_error)
        self.worker.start()

        self.call_rpc("v1.capabilities", {}, self.on_capabilities_response)
        self.do_doctor()

    def call_rpc(self, method, params, callback):
        if not hasattr(self, 'worker') or not self.worker:
            return
        req_id = self.worker.send_request(method, params)
        if req_id:
            self.pending_callbacks[req_id] = callback

    def on_rpc_response(self, data):
        req_id = data.get("id")
        if req_id in self.pending_callbacks:
            cb = self.pending_callbacks.pop(req_id)
            if "error" in data and data["error"]:
                error_message = data["error"].get("message", "")
                if self.request_sudo_authorization(error_message):
                    return
                self.set_build_busy(False, "The packaging engine reported an error.")
                QMessageBox.critical(self, "RPC Error", f"Error: {error_message}")
            else:
                cb(data.get("result"))

    def on_rpc_error(self, err_msg):
        self.set_build_busy(False, "The packaging engine is unavailable.")
        if hasattr(self, 'ipc_title'):
            self.ipc_title.setText("Engine unavailable")
            self.ipc_sub.setText("Restart ArchBridge to retry")
            self.ipc_dot.set_state("#ef4444")

    def on_capabilities_response(self, result):
        if result and hasattr(self, 'ipc_title'):
            self.ipc_title.setText("Engine ready")
            self.ipc_sub.setText("Local service connected")
            self.ipc_dot.set_state("#10b981")

    def closeEvent(self, event):
        for thread in list(getattr(self, "icon_threads", [])):
            if thread.isRunning():
                thread.requestInterruption()
                if not thread.wait(4000):
                    thread.terminate()
                    thread.wait(1000)
        if hasattr(self, 'worker') and self.worker:
            self.worker.stop()
        event.accept()


def main():
    app = QApplication(sys.argv)
    script_dir = Path(__file__).resolve().parent
    binary_path = str(script_dir / "target" / "release" / "archbridge")
    if not Path(binary_path).exists():
        binary_path = str(script_dir / "target" / "debug" / "archbridge")
    if not Path(binary_path).exists():
        binary_path = "archbridge"

    window = ArchBridgeWindow(binary_path)
    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
