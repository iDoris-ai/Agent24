"""Pipeline B: LibreOffice UNO targeted edits, run as an in-process Python macro (see README).

Inputs come from env: A_SRC, A_DOCX, A_PDF, A_PNG, A_LOG, A_TRACKED=1 for tracked mode.
"""
import time
import uno
from com.sun.star.beans import PropertyValue
from com.sun.star.text.ControlCharacter import PARAGRAPH_BREAK


def prop(n, v):
    p = PropertyValue()
    p.Name, p.Value = n, v
    return p


def main():
    import os
    src, out_docx, out_pdf, png = (os.environ[k] for k in ('A_SRC', 'A_DOCX', 'A_PDF', 'A_PNG'))
    tracked = os.environ.get('A_TRACKED') == '1'
    ctx = XSCRIPTCONTEXT.getComponentContext()
    desktop = XSCRIPTCONTEXT.getDesktop()
    t0 = time.time()
    doc = desktop.loadComponentFromURL(uno.systemPathToFileUrl(src), '_blank', 0, (prop('Hidden', True),))
    t_open = time.time() - t0
    log = []
    if tracked:
        doc.setPropertyValue('RedlineAuthor', 'Agent24') if doc.getPropertySetInfo().hasPropertyByName('RedlineAuthor') else None
        doc.RecordChanges = True
    text = doc.Text

    # 1. bookmark target
    bm = doc.Bookmarks.getByName('deadline')
    bm.Anchor.setString('2026年11月5日')
    log.append('bookmark deadline -> 2026年11月5日')

    # 2. content control tagged "contact"
    found = False
    enum = text.createEnumeration()
    while enum.hasMoreElements() and not found:
        para = enum.nextElement()
        if not para.supportsService('com.sun.star.text.Paragraph'):
            continue
        portions = para.createEnumeration()
        while portions.hasMoreElements():
            por = portions.nextElement()
            if por.TextPortionType == 'ContentControl':
                cc = por.ContentControl
                if cc.Tag == 'contact':
                    cc.setString('刘老师 021-87654321')
                    found = True
                    break
    log.append(f'content control contact found={found}')

    # 3. table cell edit, 4. append table row
    tbl = doc.TextTables.getByIndex(0)
    names = tbl.getCellNames()
    cell = next(n for n in names if tbl.getCellByName(n).getString() == '待定')
    tbl.getCellByName(cell).setString('陈静')
    log.append(f'cell {cell}: 待定 -> 陈静')
    rows = tbl.Rows
    rows.insertByIndex(rows.Count, 1)
    last = rows.Count
    for col, val in zip('ABCD', ['10月27日', '上午', '9:00-11:00', '赵磊']):
        c = tbl.getCellByName(f'{col}{last}')
        if c is not None:
            c.setString(val)
    log.append(f'row appended, now {rows.Count} rows; cells={list(tbl.getCellNames())[-4:]}')

    # 5. insert list item after "陪伴独居老人"
    enum = text.createEnumeration()
    while enum.hasMoreElements():
        para = enum.nextElement()
        if para.supportsService('com.sun.star.text.Paragraph') and para.getString() == '陪伴独居老人':
            cur = text.createTextCursorByRange(para.getEnd())
            text.insertControlCharacter(cur, PARAGRAPH_BREAK, False)
            text.insertString(cur, '社区图书整理', False)
            log.append('list item inserted')
            break

    # 6. insert image paragraph after the table
    enum = text.createEnumeration()
    nxt = None
    seen_tbl = False
    while enum.hasMoreElements():
        el = enum.nextElement()
        if el.supportsService('com.sun.star.text.TextTable'):
            seen_tbl = True
        elif seen_tbl:
            nxt = el
            break
    cur = text.createTextCursorByRange(nxt.getStart())
    text.insertControlCharacter(cur, PARAGRAPH_BREAK, False)
    cur.gotoPreviousParagraph(False)
    g = doc.createInstance('com.sun.star.text.TextGraphicObject')
    gp = ctx.ServiceManager.createInstanceWithContext('com.sun.star.graphic.GraphicProvider', ctx)
    g.Graphic = gp.queryGraphic((prop('URL', uno.systemPathToFileUrl(png)),))
    from com.sun.star.awt import Size
    g.Size = Size(4000, 2000)
    from com.sun.star.text.TextContentAnchorType import AS_CHARACTER
    g.AnchorType = AS_CHARACTER
    text.insertTextContent(cur, g, False)
    log.append('image inserted')

    t1 = time.time()
    doc.storeToURL(uno.systemPathToFileUrl(out_docx), (prop('FilterName', 'MS Word 2007 XML'),))
    doc.storeToURL(uno.systemPathToFileUrl(out_pdf), (prop('FilterName', 'writer_pdf_Export'),))
    t_save = time.time() - t1
    doc.close(True)
    open(os.environ['A_LOG'], 'w', encoding='utf-8').write('\n'.join(log) + f'\nopen {t_open:.2f}s save docx+pdf {t_save:.2f}s\n')


def run(*args):
    import os, traceback
    try:
        main()
    except Exception:
        open(os.environ['A_LOG'], 'w').write(traceback.format_exc())
    XSCRIPTCONTEXT.getDesktop().terminate()


g_exportedScripts = (run,)
