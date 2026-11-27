#!/usr/bin/env python3
"""a24-pdf2docx: convert <in.pdf> <out.docx>, stdout last line is JSON."""
import json
import logging
import os
import re
import sys
import time
import zipfile

# Same table as rust/apps/agent24-documents/src/office.rs FONT_PAIRS.
FONT_PAIRS = {
    "宋体": "Noto Serif SC",
    "SimSun": "Noto Serif SC",
    "NSimSun": "Noto Serif SC",
    "新宋体": "Noto Serif SC",
    "仿宋": "Noto Serif SC",
    "FangSong": "Noto Serif SC",
    "仿宋_GB2312": "Noto Serif SC",
    "楷体": "Noto Serif SC",
    "KaiTi": "Noto Serif SC",
    "楷体_GB2312": "Noto Serif SC",
    "Songti SC": "Noto Serif SC",
    "STSong": "Noto Serif SC",
    "黑体": "Noto Sans SC",
    "SimHei": "Noto Sans SC",
    "微软雅黑": "Noto Sans SC",
    "Microsoft YaHei": "Noto Sans SC",
    "等线": "Noto Sans SC",
    "DengXian": "Noto Sans SC",
    "PingFang SC": "Noto Sans SC",
    "STHeiti": "Noto Sans SC",
}

_FONT_ATTR = re.compile(r'(w:(?:ascii|hAnsi|eastAsia|cs))="([^"]*)"')
_FONT_NAME = re.compile(r'(<w:font\b[^>]*?\bw:name=")([^"]*)(")')
_XML_MEMBERS = ("word/document.xml", "word/styles.xml", "word/fontTable.xml")


def _emit(obj):
    print(json.dumps(obj))


def _unlink(path):
    try:
        os.remove(path)
    except OSError:
        pass


def _map_name(name):
    if name in FONT_PAIRS:
        return FONT_PAIRS[name]
    if name.startswith("FZ"):
        upper = name.upper()
        if "HT" in upper or "HEI" in upper:
            return "Noto Sans SC"
        return "Noto Serif SC"
    return name


def _map_fonts(xml):
    xml = _FONT_ATTR.sub(lambda m: f'{m.group(1)}="{_map_name(m.group(2))}"', xml)
    xml = _FONT_NAME.sub(lambda m: f"{m.group(1)}{_map_name(m.group(2))}{m.group(3)}", xml)
    return xml


def _postfix(path):
    tmp = path + ".tmp"
    try:
        with zipfile.ZipFile(path) as zin:
            with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as zout:
                for item in zin.infolist():
                    data = zin.read(item.filename)
                    if item.filename in _XML_MEMBERS:
                        x = data.decode("utf8")
                        if item.filename == "word/document.xml":
                            x = re.sub(
                                r'(<w:trHeight\b[^>]*?)w:hRule="exact"',
                                r'\1w:hRule="atLeast"',
                                x,
                            )
                            x = re.sub(
                                r'(<w:spacing\b[^>]*?)w:lineRule="exact"',
                                r'\1w:lineRule="atLeast"',
                                x,
                            )
                        x = _map_fonts(x)
                        data = x.encode("utf8")
                    zout.writestr(item, data)
        os.replace(tmp, path)
    except Exception:
        _unlink(tmp)
        raise


def _quiet_logging():
    logging.basicConfig(stream=sys.stderr, level=logging.WARNING, force=True)
    logging.getLogger().setLevel(logging.WARNING)


def main():
    if len(sys.argv) != 4 or sys.argv[1] != "convert":
        _emit({"ok": False, "error": "usage"})
        return 2

    src, dst = sys.argv[2], sys.argv[3]
    tmp_docx = dst + ".part"
    try:
        _quiet_logging()
        import pymupdf

        # PyMuPDF 的消息（含 pdf2docx `import fitz` 触发的弃用警告）默认写 stdout；
        # stdout 只留给最后一行 JSON。
        pymupdf.set_messages(stream=sys.stderr)
        doc = pymupdf.open(src)
        pages = len(doc)
        scanned = all(not page.get_text().strip() for page in doc)
        doc.close()
        if scanned:
            _emit({"ok": False, "error": "scanned_pdf", "pages": pages})
            return 3

        from pdf2docx import Converter

        _quiet_logging()
        t0 = time.time()
        converter = Converter(src)
        try:
            converter.convert(tmp_docx, parse_stream_table=False)
        finally:
            converter.close()
        _postfix(tmp_docx)
        os.replace(tmp_docx, dst)
        _emit({"ok": True, "pages": pages, "seconds": round(time.time() - t0, 2)})
        return 0
    except Exception:
        _unlink(tmp_docx)
        _unlink(tmp_docx + ".tmp")
        _emit({"ok": False, "error": "convert_failed"})
        return 4


if __name__ == "__main__":
    sys.exit(main())
