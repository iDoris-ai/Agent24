"""Feature probe for a DOCX: prints a JSON fingerprint of the structures we care about.

usage: python3 -I probe.py file.docx [...]
"""
import json
import re
import sys
import zipfile
import xml.etree.ElementTree as ET

NS = {'w': 'http://schemas.openxmlformats.org/wordprocessingml/2006/main'}
WN = '{%s}' % NS['w']


def text_of(el):
    out = []
    for n in el.iter():
        if n.tag == WN + 't' and n.text:
            out.append(n.text)
        elif n.tag == WN + 'tab':
            out.append('\t')
    return ''.join(out)


def visible_text(el):
    # text with tracked deletions excluded (w:delText is never counted)
    return text_of(el)


def part(z, name):
    try:
        return ET.fromstring(z.read(name))
    except KeyError:
        return None


def probe(path):
    z = zipfile.ZipFile(path)
    names = z.namelist()
    doc = part(z, 'word/document.xml')
    body = doc.find('w:body', NS)
    paras = body.findall('.//w:p', NS)
    texts = [visible_text(p) for p in paras]
    find = lambda s: next((t for t in texts if s in t), None)
    deadline_p = next((p for p in paras if '报名截止日期' in visible_text(p)), None)
    bold_runs = []
    if deadline_p is not None:
        for r in deadline_p.iter(WN + 'r'):
            b = r.find('w:rPr/w:b', NS) is not None
            t = text_of(r)
            if t:
                bold_runs.append((t, b))
    headers = [n for n in names if re.match(r'word/header\d*\.xml', n)]
    footers = [n for n in names if re.match(r'word/footer\d*\.xml', n)]
    hf_text = {n: text_of(part(z, n)) for n in headers + footers}
    footer_fields = sum(1 for n in footers for i in part(z, n).iter(WN + 'instrText') if 'PAGE' in (i.text or '')) + \
        sum(1 for n in footers for f in part(z, n).iter(WN + 'fldSimple') if 'PAGE' in f.get(WN + 'instr', ''))
    comments = part(z, 'word/comments.xml')
    footnotes = part(z, 'word/footnotes.xml')
    tbl = body.find('.//w:tbl', NS)
    cells = []
    if tbl is not None:
        for tr in tbl.findall('w:tr', NS):
            cells.append([text_of(tc) for tc in tr.findall('w:tc', NS)])
    east = sorted({f.get(WN + 'eastAsia') for f in doc.iter(WN + 'rFonts') if f.get(WN + 'eastAsia')})
    sty = part(z, 'word/styles.xml')
    east_styles = sorted({f.get(WN + 'eastAsia') for f in sty.iter(WN + 'rFonts') if f.get(WN + 'eastAsia')}) if sty is not None else []
    return {
        'file': path.split('/')[-1],
        'parts': len(names),
        'paragraphs_body': len(paras),
        'deadline_paragraph': find('报名截止日期'),
        'deadline_runs_bold': bold_runs,
        'bookmarks': [b.get(WN + 'name') for b in body.iter(WN + 'bookmarkStart') if not b.get(WN + 'name', '').startswith('_')],
        'sdt_tags': [t.get(WN + 'val') for t in body.iter(WN + 'tag')],
        'sdt_contact_text': next((text_of(s.find('w:sdtContent', NS)) for s in body.iter(WN + 'sdt')), None),
        'comments': [text_of(c) for c in comments.findall('w:comment', NS)] if comments is not None else [],
        'comment_anchor': find('身份证'),
        'tracked_ins': [text_of(i) for i in body.iter(WN + 'ins')],
        'tracked_del': [''.join(t.text or '' for t in d.iter(WN + 'delText')) for d in body.iter(WN + 'del')],
        'footnotes': [text_of(f).strip() for f in footnotes.findall('w:footnote', NS) if text_of(f).strip()] if footnotes is not None else [],
        'list_items': [visible_text(p) for p in paras if p.find('w:pPr/w:numPr', NS) is not None],
        'table_cells': cells,
        'gridSpan': [g.get(WN + 'val') for g in body.iter(WN + 'gridSpan')],
        'vMerge': len(list(body.iter(WN + 'vMerge'))),
        'header_footer_text': hf_text,
        'footer_PAGE_field': footer_fields,
        'eastAsia_fonts_doc': east,
        'eastAsia_fonts_styles': east_styles,
        'images': len([n for n in names if n.startswith('word/media/')]),
        'tail_paragraphs': [t for t in texts if t][-3:],
    }


if __name__ == '__main__':
    print(json.dumps([probe(p) for p in sys.argv[1:]], ensure_ascii=False, indent=1))
