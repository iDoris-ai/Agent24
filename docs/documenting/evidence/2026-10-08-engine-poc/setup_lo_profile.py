"""Create an isolated LibreOffice profile with bundled Noto CJK fonts and a CJK font substitution table.

usage: python3 -I setup_lo_profile.py <profile_dir> <fonts_dir> <lo_macro.py>
"""
import pathlib
import shutil
import sys

PAIRS = [('SimSun', 'Noto Serif SC'), ('宋体', 'Noto Serif SC'), ('NSimSun', 'Noto Serif SC'),
         ('FangSong', 'Noto Serif SC'), ('仿宋', 'Noto Serif SC'), ('仿宋_GB2312', 'Noto Serif SC'),
         ('KaiTi', 'Noto Serif SC'), ('楷体', 'Noto Serif SC'),
         ('SimHei', 'Noto Sans SC'), ('黑体', 'Noto Sans SC'), ('Microsoft YaHei', 'Noto Sans SC'),
         ('微软雅黑', 'Noto Sans SC'), ('DengXian', 'Noto Sans SC'), ('等线', 'Noto Sans SC')]

profile, fonts, macro = map(pathlib.Path, sys.argv[1:4])
user = profile / 'user'
(user / 'fonts').mkdir(parents=True, exist_ok=True)
(user / 'Scripts' / 'python').mkdir(parents=True, exist_ok=True)
for f in fonts.glob('*.otf'):
    shutil.copy(f, user / 'fonts' / f.name)
shutil.copy(macro, user / 'Scripts' / 'python' / macro.name)
items = ''.join(
    f'<item oor:path="/org.openoffice.Office.Common/Font/Substitution/FontPairs"><node oor:name="_{i}" oor:op="replace">'
    f'<prop oor:name="Always" oor:op="fuse"><value>true</value></prop>'
    f'<prop oor:name="OnScreenOnly" oor:op="fuse"><value>false</value></prop>'
    f'<prop oor:name="ReplaceFont" oor:op="fuse"><value>{a}</value></prop>'
    f'<prop oor:name="SubstituteFont" oor:op="fuse"><value>{b}</value></prop></node></item>'
    for i, (a, b) in enumerate(PAIRS))
(user / 'registrymodifications.xcu').write_text(
    '<?xml version="1.0" encoding="UTF-8"?><oor:items xmlns:oor="http://openoffice.org/2001/registry" '
    'xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">'
    '<item oor:path="/org.openoffice.Office.Common/Font/Substitution"><prop oor:name="Replacement" oor:op="fuse">'
    f'<value>true</value></prop></item>{items}</oor:items>', encoding='utf-8')
