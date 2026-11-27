"""a24-pdf2docx 回归测试（Loop 12 L12-1）。

对一个已构建的引擎运行：
    A24_PDF2DOCX=<dist>/a24-pdf2docx/a24-pdf2docx python3 -m unittest discover -s tools/engines/pdf2docx/tests
不设 A24_PDF2DOCX 时，用当前 python 直接运行 a24_pdf2docx.py（要求环境里已装钉版依赖并打过补丁）。

只用标准库：zipfile 读 word/document.xml，正则数字符。
期望值来自独立参照：PDF 里的字符数用仓内 PDFKit 解析器（agent24-documents-pdfkit parse）数出，
不用 PyMuPDF，避免和被测对象同库自证（裁决记录 2026-11-28-pdf2word-component C3）。
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, 'fixtures')
REPO = os.path.abspath(os.path.join(HERE, '..', '..', '..', '..'))
PUTUO = os.path.join(REPO, 'docs', 'documenting', 'samples', 's01', 's01-05-zh-putuo-fee-2024', 'source.pdf')
CLI = os.path.join(HERE, '..', 'a24_pdf2docx.py')

# office.rs FONT_PAIRS 里的中文字体名：转出的 DOCX 里不应再出现。
SOURCE_FONTS = ['宋体', 'SimSun', 'NSimSun', '新宋体', '仿宋', 'FangSong', '仿宋_GB2312', '楷体', 'KaiTi',
                '楷体_GB2312', 'Songti SC', 'STSong', '黑体', 'SimHei', '微软雅黑', 'Microsoft YaHei',
                '等线', 'DengXian', 'PingFang SC', 'STHeiti']


def run(src, dst):
    engine = os.environ.get('A24_PDF2DOCX')
    cmd = [engine] if engine else [sys.executable, CLI]
    proc = subprocess.run(cmd + ['convert', src, dst], capture_output=True, text=True, timeout=300)
    lines = [l for l in proc.stdout.splitlines() if l.strip()]
    result = json.loads(lines[-1]) if lines else None
    return proc.returncode, result


def document_xml(path):
    with zipfile.ZipFile(path) as z:
        return z.read('word/document.xml').decode('utf8')


def text_of(xml):
    return ''.join(re.findall(r'<w:t(?:\s[^>]*)?>([^<]*)</w:t>', xml))


class Converted:
    """每个样本只转一次。"""
    _cache = {}
    _tmp = tempfile.mkdtemp(prefix='a24-pdf2docx-test-')

    @classmethod
    def get(cls, src):
        if src not in cls._cache:
            dst = os.path.join(cls._tmp, os.path.basename(src) + '.docx')
            code, result = run(src, dst)
            cls._cache[src] = (code, result, dst)
        return cls._cache[src]


class OutputContract(unittest.TestCase):
    def test_success_prints_one_json_line(self):
        code, result, dst = Converted.get(os.path.join(FIXTURES, 'contract-realistic.pdf'))
        self.assertEqual(code, 0)
        self.assertEqual(result['ok'], True)
        self.assertEqual(result['pages'], 1)
        self.assertIsInstance(result['seconds'], (int, float))
        self.assertTrue(zipfile.is_zipfile(dst))

    def test_scanned_pdf_is_refused(self):
        code, result, dst = Converted.get(os.path.join(FIXTURES, 'scan.pdf'))
        self.assertEqual(code, 3)
        self.assertEqual(result, {'ok': False, 'error': 'scanned_pdf', 'pages': 1})
        self.assertFalse(os.path.exists(dst))

    def test_unreadable_input_fails_cleanly(self):
        bad = os.path.join(Converted._tmp, 'not-a.pdf')
        with open(bad, 'wb') as f:
            f.write(b'this is not a pdf')
        code, result = run(bad, bad + '.docx')
        self.assertEqual(code, 4)
        self.assertEqual(result['ok'], False)
        self.assertEqual(result['error'], 'convert_failed')
        self.assertFalse(os.path.exists(bad + '.docx'))

    def test_usage(self):
        engine = os.environ.get('A24_PDF2DOCX')
        cmd = [engine] if engine else [sys.executable, CLI]
        proc = subprocess.run(cmd + ['nope'], capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 2)
        self.assertEqual(json.loads(proc.stdout.strip().splitlines()[-1]), {'ok': False, 'error': 'usage'})


class Fix1LineEndPunctuation(unittest.TestCase):
    """修复 1：行尾挂出的中文标点不再丢（PDFKit 数出的 PDF 字符数）。"""

    def test_heilongjiang_counts_match_the_pdf(self):
        code, _, dst = Converted.get(os.path.join(FIXTURES, 'heilongjiang-decoration.pdf'))
        self.assertEqual(code, 0)
        text = text_of(document_xml(dst))
        self.assertEqual(text.count('，'), 298)
        self.assertEqual(text.count('、'), 191)
        self.assertEqual(text.count('间'), 35)
        self.assertIn('未施工，不计算', re.sub(r'\s+', '', text))


class Fix2NumberedListsStayParagraphs(unittest.TestCase):
    """修复 2：编号条款不被识别成无边框表格。"""

    def test_contract_has_only_the_real_table(self):
        code, _, dst = Converted.get(os.path.join(FIXTURES, 'contract-realistic.pdf'))
        self.assertEqual(code, 0)
        xml = document_xml(dst)
        self.assertEqual(xml.count('<w:tbl>'), 1)
        tables = ''.join(re.findall(r'<w:tbl>.*?</w:tbl>', xml, flags=re.S))
        for clause in ('租赁期限自', '每月租金为人民币', '押金为两个月租金'):
            self.assertIn(clause, re.sub(r'\s+', '', text_of(xml)))
            self.assertNotIn(clause, re.sub(r'\s+', '', text_of(tables)))


class Fix3NoFixedHeights(unittest.TestCase):
    """修复 3：不留固定行高、固定行距（截断的成因），表格里的文号完整、不交错。"""

    def test_putuo_rows_and_lines_can_grow(self):
        code, _, dst = Converted.get(PUTUO)
        self.assertEqual(code, 0)
        xml = document_xml(dst)
        self.assertNotRegex(xml, r'<w:trHeight\b[^>]*w:hRule="exact"')
        self.assertNotRegex(xml, r'<w:spacing\b[^>]*w:lineRule="exact"')
        cells = re.sub(r'\s+', '', text_of(''.join(re.findall(r'<w:tbl>.*?</w:tbl>', xml, flags=re.S))))
        for number in ('建城[1993]410号', '财办税[2020]13号', '沪价费[1995]195号', '沪交停[2005]621号'):
            self.assertIn(number, cells)


class FontNamesMapped(unittest.TestCase):
    """中文字体名换成随引擎附带的 Noto（office.rs FONT_PAIRS 同一张表）。"""

    def test_putuo_names_no_source_cjk_font(self):
        code, _, dst = Converted.get(PUTUO)
        self.assertEqual(code, 0)
        xml = document_xml(dst)
        named = set(re.findall(r'w:(?:ascii|hAnsi|eastAsia|cs)="([^"]+)"', xml))
        self.assertFalse(named & set(SOURCE_FONTS), named & set(SOURCE_FONTS))
        # 方正系列（FZSSK 书宋、FZXBSJW 小标宋…）也不会在用户机器上，同样换掉。
        self.assertFalse({n for n in named if n.startswith('FZ')}, named)
        self.assertTrue(named & {'Noto Serif SC', 'Noto Sans SC'}, named)


if __name__ == '__main__':
    unittest.main()
