#!/usr/bin/env python3
"""Render the Steam library artwork for the 3DS app into launcher/steam/ with headless Chrome:
  portrait.png 600x900 (library grid), wide.png 920x430 (recent games), hero.png 1920x620
  (banner behind the game page), logo.png 1000x400 (transparent, drawn over the hero).

  tools/make-steam-art.py [path/to/chrome]
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "launcher/steam"
ICON = (ROOT / "launcher/icons/icon.svg").read_text()
FONT = (ROOT / "launcher/ui/fonts/Nunito.ttf").as_uri()

BASE = f"""<!doctype html><meta charset="utf-8"><style>
@font-face {{ font-family: Nunito; src: url("{FONT}"); font-weight: 200 1000; }}
* {{ box-sizing: border-box; margin: 0; }}
html, body {{ width: 100%; height: 100%; overflow: hidden; font-family: Nunito; }}
.bg {{ background:
    repeating-linear-gradient(-45deg, transparent 0 28px, rgba(255,255,255,.07) 28px 56px),
    radial-gradient(120% 90% at 30% 0%, #4fd6ff 0%, #19b5ec 45%, #0a7fc4 100%); }}
.word {{ color: #fff; font-weight: 1000; letter-spacing: -.02em; line-height: .9;
         text-shadow: 0 .04em 0 #0670a8, 0 .08em .25em rgba(4, 50, 90, .35); }}
.sub {{ color: rgba(255,255,255,.88); font-weight: 800; }}
.icon svg {{ width: 100%; height: 100%; display: block; filter: drop-shadow(0 18px 30px rgba(4,50,90,.35)); }}
.tile {{ position: absolute; border-radius: 22%; opacity: .9; box-shadow: 0 10px 30px rgba(4,50,90,.25); }}
</style>"""

ART = {
    "portrait.png": (600, 900, f"""
<body class="bg" style="display:flex;flex-direction:column;align-items:center;justify-content:center;gap:40px">
  <div class="icon" style="width:360px;height:360px">{ICON}</div>
  <div class="word" style="font-size:190px">3DS</div>
  <div class="sub" style="font-size:40px">Your game library</div>
</body>"""),
    "wide.png": (920, 430, f"""
<body class="bg" style="display:flex;align-items:center;justify-content:center;gap:50px">
  <div class="icon" style="width:300px;height:300px">{ICON}</div>
  <div><div class="word" style="font-size:200px">3DS</div>
       <div class="sub" style="font-size:38px;margin-top:18px">Your game library</div></div>
</body>"""),
    # Steam draws the logo over the left of the hero, so the right side carries the decoration.
    "hero.png": (1920, 620, """
<body class="bg" style="position:relative">
  <div class="tile" style="left:1180px;top:90px;width:170px;height:170px;background:#3fc1f0;transform:rotate(-8deg)"></div>
  <div class="tile" style="left:1400px;top:60px;width:210px;height:210px;background:#ffb84a;transform:rotate(6deg)"></div>
  <div class="tile" style="left:1660px;top:150px;width:160px;height:160px;background:#ff6f91;transform:rotate(-4deg)"></div>
  <div class="tile" style="left:1260px;top:330px;width:200px;height:200px;background:#7ee0a8;transform:rotate(5deg)"></div>
  <div class="tile" style="left:1520px;top:350px;width:180px;height:180px;background:#b49cff;transform:rotate(-7deg)"></div>
  <div class="tile" style="left:1760px;top:400px;width:150px;height:150px;background:#ffffff;opacity:.5;transform:rotate(9deg)"></div>
</body>"""),
    "logo.png": (1000, 400, f"""
<body style="display:flex;align-items:center;gap:44px;padding:0 30px;background:transparent">
  <div class="icon" style="width:300px;height:300px;flex:none">{ICON}</div>
  <div class="word" style="font-size:230px">3DS</div>
</body>"""),
}


def main():
    chrome = sys.argv[1] if len(sys.argv) > 1 else (
        shutil.which("google-chrome") or shutil.which("chromium") or shutil.which("chromium-browser"))
    if not chrome:
        sys.exit("needs Chrome or Chromium")
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        for name, (w, h, body) in ART.items():
            page = Path(tmp) / (name + ".html")
            page.write_text(BASE + body)
            subprocess.run([chrome, "--headless", "--disable-gpu", "--hide-scrollbars",
                            "--force-device-scale-factor=1", f"--window-size={w},{h}",
                            "--default-background-color=00000000", f"--user-data-dir={tmp}/profile",
                            f"--screenshot={OUT / name}", page.as_uri()],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            print(f"{OUT / name}  {w}x{h}")


if __name__ == "__main__":
    main()
