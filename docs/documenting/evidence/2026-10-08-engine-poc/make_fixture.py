"""Build a hand-written OOXML template covering features the Documenting slices care about.

Features: CJK + Latin fonts, header/footer with PAGE field, bookmark target, plain-text
content control (SDT), comment, pre-existing tracked insertion/deletion, footnote,
numbered list, table with horizontal (gridSpan) and vertical (vMerge) merges, mixed
bold/regular runs in one paragraph.
"""
import sys
import zipfile

W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'

def rpr(bold=False):
    return '<w:rPr>' + ('<w:b/><w:bCs/>' if bold else '') + '</w:rPr>'

def run(t, bold=False):
    return f'<w:r>{rpr(bold)}<w:t xml:space="preserve">{t}</w:t></w:r>'

def p(content, style=None, extra_ppr=''):
    ppr = (f'<w:pStyle w:val="{style}"/>' if style else '') + extra_ppr
    return f'<w:p><w:pPr>{ppr}</w:pPr>{content}</w:p>' if ppr else f'<w:p>{content}</w:p>'

def li(t):
    return p(run(t), extra_ppr='<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>')

def tc(t, w=2000, extra=''):
    return f'<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/>{extra}</w:tcPr>{p(run(t)) if t is not None else "<w:p/>"}</w:tc>'

body = ''.join([
    p(run('关于开展 2026 年秋季社区志愿服务的通知'), 'Title'),
    p(run('Notice on the 2026 Autumn Community Volunteer Programme'), 'Subtitle'),
    p(run('一、活动概述'), 'Heading1'),
    p(run('为丰富社区文化生活，现组织秋季志愿服务。本活动面向全体居民，')
      + run('不收取任何费用', True) + run('。Participation is free of charge.')),
    p(run('二、报名方式'), 'Heading1'),
    p(run('报名截止日期：', True)
      + '<w:bookmarkStart w:id="0" w:name="deadline"/>' + run('2026年10月20日') + '<w:bookmarkEnd w:id="0"/>'
      + run('（逾期不予受理）。')),
    p(run('联系人：')
      + '<w:sdt><w:sdtPr><w:alias w:val="联系人"/><w:tag w:val="contact"/><w:id w:val="101"/><w:text/></w:sdtPr>'
        '<w:sdtContent>' + run('张老师 021-12345678') + '</w:sdtContent></w:sdt>'),
    p('<w:commentRangeStart w:id="0"/>' + run('报名需携带身份证原件')
      + '<w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r>'
      + run('，未成年人须由监护人陪同')
      + '<w:r><w:footnoteReference w:id="1"/></w:r>' + run('。')),
    p(run('集合地点：社区活动中心')
      + '<w:del w:id="10" w:author="王五" w:date="2026-10-01T09:00:00Z"><w:r><w:delText>二楼</w:delText></w:r></w:del>'
      + '<w:ins w:id="11" w:author="王五" w:date="2026-10-01T09:00:00Z">' + run('一楼大厅') + '</w:ins>'
      + run('。')),
    p(run('三、服务项目'), 'Heading1'),
    li('清洁公共区域（Public area cleaning）'),
    li('陪伴独居老人'),
    li('垃圾分类宣传'),
    p(run('四、排班表'), 'Heading1'),
    '<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="8000" w:type="dxa"/>'
    '<w:tblBorders><w:top w:val="single" w:sz="4"/><w:left w:val="single" w:sz="4"/><w:bottom w:val="single" w:sz="4"/>'
    '<w:right w:val="single" w:sz="4"/><w:insideH w:val="single" w:sz="4"/><w:insideV w:val="single" w:sz="4"/></w:tblBorders></w:tblPr>'
    '<w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>'
    '<w:tr>' + tc('日期') + tc('时段 / Shift', 4000, '<w:gridSpan w:val="2"/>') + tc('负责人') + '</w:tr>'
    '<w:tr>' + tc('10月25日', extra='<w:vMerge w:val="restart"/>') + tc('上午') + tc('9:00-11:00') + tc('李明') + '</w:tr>'
    '<w:tr>' + tc(None, extra='<w:vMerge/>') + tc('下午') + tc('14:00-16:00') + tc('待定') + '</w:tr>'
    '<w:tr>' + tc('10月26日') + tc('全天') + tc('9:00-16:00') + tc('Alice Wang') + '</w:tr>'
    '</w:tbl>',
    p(run('五、其他事项'), 'Heading1'),
    p(run('如遇恶劣天气，活动顺延，具体时间另行通知。In case of severe weather, the event will be postponed.')),
    p(run('社区居委会'), extra_ppr='<w:jc w:val="right"/>'),
    p(run('2026年10月8日'), extra_ppr='<w:jc w:val="right"/>'),
    '<w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/><w:footerReference w:type="default" r:id="rIdF1"/>'
    '<w:footnotePr><w:numFmt w:val="decimal"/></w:footnotePr>'
    '<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>',
])

document = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}</w:body></w:document>'

styles = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {W}>
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="SimSun" w:cs="Times New Roman"/>
<w:sz w:val="24"/><w:szCs w:val="24"/><w:lang w:val="en-US" w:eastAsia="zh-CN"/></w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="300" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:pPr><w:jc w:val="center"/></w:pPr>
<w:rPr><w:rFonts w:eastAsia="SimHei"/><w:b/><w:sz w:val="36"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:basedOn w:val="Normal"/><w:pPr><w:jc w:val="center"/></w:pPr>
<w:rPr><w:i/><w:sz w:val="22"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="240"/><w:outlineLvl w:val="0"/></w:pPr>
<w:rPr><w:rFonts w:eastAsia="SimHei"/><w:b/><w:sz w:val="28"/></w:rPr></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/></w:style>
<w:style w:type="character" w:styleId="FootnoteReference"><w:name w:val="footnote reference"/><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>
</w:styles>'''

numbering = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering {W}>
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/>
<w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>'''

header = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr {W}>{p(run("某某社区服务中心 · 内部通知"), extra_ppr="<w:jc w:val=\"center\"/>")}</w:hdr>'
footer = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr {W}><w:p><w:pPr><w:jc w:val="center"/></w:pPr>'
          + run('第 ') + '<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>'
          '<w:r><w:fldChar w:fldCharType="separate"/></w:r>' + run('1') + '<w:r><w:fldChar w:fldCharType="end"/></w:r>' + run(' 页') + '</w:p></w:ftr>')
comments = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments {W}>'
            '<w:comment w:id="0" w:author="审核人" w:date="2026-10-02T10:00:00Z" w:initials="SH">'
            + p(run('是否也接受护照？')) + '</w:comment></w:comments>')
footnotes = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes {W}>'
             '<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>'
             '<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>'
             '<w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r>' + run(' 未成年人指未满 18 周岁者。') + '</w:p></w:footnote></w:footnotes>')

R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships'
rels = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rIdS" Type="{R}/styles" Target="styles.xml"/>
<Relationship Id="rIdN" Type="{R}/numbering" Target="numbering.xml"/>
<Relationship Id="rIdH1" Type="{R}/header" Target="header1.xml"/>
<Relationship Id="rIdF1" Type="{R}/footer" Target="footer1.xml"/>
<Relationship Id="rIdC" Type="{R}/comments" Target="comments.xml"/>
<Relationship Id="rIdFN" Type="{R}/footnotes" Target="footnotes.xml"/></Relationships>'''
pkg_rels = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>'''
WM = 'application/vnd.openxmlformats-officedocument.wordprocessingml'
ctypes = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="{WM}.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="{WM}.styles+xml"/>
<Override PartName="/word/numbering.xml" ContentType="{WM}.numbering+xml"/>
<Override PartName="/word/header1.xml" ContentType="{WM}.header+xml"/>
<Override PartName="/word/footer1.xml" ContentType="{WM}.footer+xml"/>
<Override PartName="/word/comments.xml" ContentType="{WM}.comments+xml"/>
<Override PartName="/word/footnotes.xml" ContentType="{WM}.footnotes+xml"/></Types>'''

with zipfile.ZipFile(sys.argv[1], 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('[Content_Types].xml', ctypes)
    z.writestr('_rels/.rels', pkg_rels)
    z.writestr('word/_rels/document.xml.rels', rels)
    for name, xml in [('document', document), ('styles', styles), ('numbering', numbering), ('header1', header),
                      ('footer1', footer), ('comments', comments), ('footnotes', footnotes)]:
        z.writestr(f'word/{name}.xml', xml)
