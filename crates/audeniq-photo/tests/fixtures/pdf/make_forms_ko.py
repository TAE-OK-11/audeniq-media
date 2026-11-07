# Builds test PDFs: non-embedded Korean CID font, Helvetica/Arial text, form widgets with and without /AP.
import sys
def pdf(objs, root=1):
    out=b'%PDF-1.7\n%\xe2\xe3\xcf\xd3\n'; offs=[]
    for i,o in enumerate(objs,1):
        offs.append(len(out)); out+=f'{i} 0 obj\n'.encode()+o+b'\nendobj\n'
    x=len(out); out+=f'xref\n0 {len(objs)+1}\n0000000000 65535 f \n'.encode()
    for o in offs: out+=f'{o:010d} 00000 n \n'.encode()
    out+=f'trailer\n<< /Size {len(objs)+1} /Root {root} 0 R >>\nstartxref\n{x}\n%%EOF\n'.encode(); return out
def stream(d, body): return f'<< {d} /Length {len(body)} >>\nstream\n'.encode()+body+b'\nendstream'
def ucs2(s): return '<'+s.encode('utf-16-be').hex().upper()+'>'
text = ('BT /K1 22 Tf 72 720 Td '+ucs2('계약서: 아티스트 동의 확인')+' Tj ET\n'
        'BT /K2 16 Tf 72 690 Td '+ucs2('굵은 글씨 (Bold) 서명란')+' Tj ET\n'
        'BT /F1 14 Tf 72 650 Td (Helvetica: Agreement terms 2026) Tj ET\n'
        'BT /F2 14 Tf 72 630 Td (Arial non-embedded TrueType text) Tj ET\n'
        'BT /F3 14 Tf 72 610 Td (Times-Roman stays serif) Tj ET\n'
        'BT 72 590 Td (No Tf: default font) Tj ET\n').encode()
# form widgets
on_ap = stream('/Type /XObject /Subtype /Form /BBox [0 0 14 14]', b'0 0 1 rg 2 2 10 10 re f')
off_ap = stream('/Type /XObject /Subtype /Form /BBox [0 0 14 14]', b'0 g 0.5 0.5 13 13 re S')
objs = [
 b'<< /Type /Catalog /Pages 2 0 R /AcroForm 20 0 R >>',                     # 1
 b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',                               # 2
 b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /K1 5 0 R /K2 8 0 R /F1 9 0 R /F2 10 0 R /F3 11 0 R >> >> /Annots [12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R 19 0 R 23 0 R] >>', # 3
 stream('', text),                                                            # 4
 b'<< /Type /Font /Subtype /Type0 /BaseFont /Batang /Encoding /UniKS-UCS2-H /DescendantFonts [6 0 R] >>', # 5
 b'<< /Type /Font /Subtype /CIDFontType0 /BaseFont /Batang /CIDSystemInfo << /Registry (Adobe) /Ordering (Korea1) /Supplement 2 >> /FontDescriptor 7 0 R /DW 1000 >>', # 6
 b'<< /Type /FontDescriptor /FontName /Batang /Flags 6 /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >>', # 7
 b'<< /Type /Font /Subtype /Type0 /BaseFont /MalgunGothic-Bold /Encoding /UniKS-UCS2-H /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MalgunGothic-Bold /CIDSystemInfo << /Registry (Adobe) /Ordering (Korea1) /Supplement 2 >> /FontDescriptor << /Type /FontDescriptor /FontName /MalgunGothic-Bold /Flags 4 /FontWeight 700 /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 120 >> >>] >>', # 8
 b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',                  # 9
 b'<< /Type /Font /Subtype /TrueType /BaseFont /ArialMT /FirstChar 32 /LastChar 126 /FontDescriptor << /Type /FontDescriptor /FontName /ArialMT /Flags 32 /FontBBox [0 -210 1000 900] /ItalicAngle 0 /Ascent 900 /Descent -210 /CapHeight 716 /StemV 80 >> >>', # 10
 b'<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>',                # 11
 # 12: checkbox with own AP, checked
 b'<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree_ap) /V /Yes /AS /Yes /Rect [72 540 86 554] /AP << /N << /Yes 21 0 R /Off 22 0 R >> >> /F 4 /P 3 0 R >>',
 # 13: checkbox without AP, checked
 b'<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree_noap) /V /Yes /AS /Yes /Rect [72 515 88 531] /F 4 /P 3 0 R /MK << /CA (4) >> >>',
 # 14: checkbox without AP, unchecked
 b'<< /Type /Annot /Subtype /Widget /FT /Btn /T (marketing) /V /Off /AS /Off /Rect [72 490 88 506] /F 4 /P 3 0 R >>',
 # 15: text field without AP (Korean value)
 b'<< /Type /Annot /Subtype /Widget /FT /Tx /T (artist_name) /V <FEFF'+'홍길동 (Hong Gildong)'.encode('utf-16-be').hex().upper().encode()+b'> /DA (/Helv 12 Tf 0 g) /Rect [120 450 400 470] /F 4 /P 3 0 R >>',
 # 16,17: radio group kids without AP (second selected)
 b'<< /Type /Annot /Subtype /Widget /Parent 24 0 R /AS /Off /Rect [72 410 88 426] /F 4 /P 3 0 R /AP << /N << /Off null >> >> >>',
 b'<< /Type /Annot /Subtype /Widget /Parent 24 0 R /AS /Excl /Rect [120 410 136 426] /F 4 /P 3 0 R >>',
 # 18: combo box without AP
 b'<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (territory) /V (Worldwide) /Opt [(Korea) (Worldwide)] /DA (/Helv 11 Tf 0 0 1 rg) /Rect [120 370 300 388] /F 4 /P 3 0 R >>',
 # 19: unsigned signature field
 b'<< /Type /Annot /Subtype /Widget /FT /Sig /T (signature) /Rect [120 320 300 350] /F 4 /P 3 0 R >>',
 b'<< /Fields [12 0 R 13 0 R 14 0 R 15 0 R 24 0 R 18 0 R 19 0 R 23 0 R] /NeedAppearances true /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 9 0 R >> >> >>', # 20
 on_ap, off_ap,                                                               # 21, 22
 # 23: multiline text without AP
 b'<< /Type /Annot /Subtype /Widget /FT /Tx /Ff 4096 /T (notes) /V (First line\\rSecond line of notes) /DA (/Helv 10 Tf 0 g) /Rect [120 230 400 290] /F 4 /P 3 0 R >>',
 b'<< /FT /Btn /Ff 49152 /T (plan) /V /Excl /Kids [16 0 R 17 0 R] >>',      # 24
]
open(sys.argv[1],'wb').write(pdf(objs))
