"""One window for players: pick your own Soulframe folder, set up the private copy, run the server, play.
Run from the repo (pythonw tools/server_gui.pyw), or as the .exe / .AppImage that build.ps1 / build.sh make.

Nothing is downloaded from Digital Extremes: the game comes from the folder you pick (any copy of build P16H6 with
Soulframe.x64.exe in it), and setup copies it to the private folder that only talks to this server.

Bundled (.exe / .AppImage): the server's files are copied out of the read-only bundle into a folder of their own
(DATA below) on every start, so accounts, certs/ and captures/ stay there between runs and updates.
"""
import hashlib, json, os, pathlib, queue, runpy, shutil, subprocess, sys, threading
import tkinter as tk
from tkinter import filedialog, messagebox, simpledialog, ttk
# The server is run from source (runpy), so PyInstaller can't see what it imports: name it here.
import datetime, gzip, hmac, http.client, http.server, math, random, re, secrets, socket, ssl, struct, tempfile, time  # noqa: F401
import urllib.parse, zlib, collections, functools, ctypes, glob  # noqa: F401
if sys.platform == 'win32':
    import ctypes.wintypes  # noqa: F401  (devgui.pyw)

P16H6 = '3be7694b589a7e2ae92a53b3e26ed8c3bcfac8c34ff25d69045df374b35e4765'  # Soulframe.x64.exe, build 2026.10.08.15.20
WINDOWS = sys.platform == 'win32'
FROZEN = getattr(sys, 'frozen', False)
REPO = pathlib.Path(__file__).resolve().parent.parent
if FROZEN:
    BUNDLE = pathlib.Path(sys._MEIPASS)
    DATA = (pathlib.Path(os.environ['LOCALAPPDATA']) / 'SoulframePrivateServer' if WINDOWS else
            pathlib.Path(os.environ.get('XDG_DATA_HOME') or pathlib.Path.home() / '.local/share') / 'soulframe-private-server')
    JOIN = DATA / 'join'
else:
    DATA, JOIN = REPO, REPO / 'release/join'
SETTINGS = DATA / 'data/gui.json'
COPY_NAME = 'SoulframePrivate'  # the copy always gets a folder of its own: setup takes it over (chown on Linux)
DEFAULT_COPY = pathlib.Path(os.environ.get('USERPROFILE', '')) / COPY_NAME if WINDOWS else pathlib.Path('/home/sfplay') / COPY_NAME
NO_WINDOW = 0x08000000 if WINDOWS else 0  # CREATE_NO_WINDOW
# PyInstaller points LD_LIBRARY_PATH at its bundled libs; system programs we start (pkexec, sudo, wine) need their own.
ENV = dict(os.environ)
if FROZEN and not WINDOWS:
    ENV.pop('LD_LIBRARY_PATH', None)
    if 'LD_LIBRARY_PATH_ORIG' in ENV:
        ENV['LD_LIBRARY_PATH'] = ENV.pop('LD_LIBRARY_PATH_ORIG')


def unpack():
    """Bundled only: refresh the server's code and data in DATA; data/, certs/ and captures/ are left alone."""
    for d in 'tools', 'fixtures', 'gamedata', 'join', 'dev', 'bootstrap':  # dev + bootstrap/src/imgui.rs: devlua.py
        if (BUNDLE / d).exists():
            shutil.copytree(BUNDLE / d, DATA / d, dirs_exist_ok=True)
    for x in [*JOIN.glob('*.sh'), DATA / 'tools/luau/luau-compile']:
        if x.exists():
            x.chmod(0o755)


def run_script(name, args):
    """`<exe> --server|--devgui [flags]`: the bundled exe doubles as these processes (there is no python.exe to start)."""
    if sys.stdout:
        sys.stdout.reconfigure(line_buffering=True)
    sys.path.insert(0, str(DATA / 'tools'))
    sys.argv = [str(DATA / 'tools' / name), *args]
    runpy.run_path(sys.argv[0], run_name='__main__')


MODES = {'--server': 'dummy_server.py', '--devgui': 'devgui.pyw', '--devlua': 'devlua.py'}


def script(mode, *args):
    """The command line for one of MODES: through this exe when bundled, else tools/<script> with this Python."""
    return [sys.executable, mode, *args] if FROZEN else [sys.executable, '-u', str(REPO / 'tools' / MODES[mode]), *args]


def game_build(folder: str) -> str:
    exe = pathlib.Path(folder) / 'Soulframe.x64.exe'
    if not exe.is_file():
        return 'no Soulframe.x64.exe in this folder'
    with open(exe, 'rb') as f:
        ok = hashlib.file_digest(f, 'sha256').hexdigest() == P16H6
    return 'ok' if ok else 'wrong game version (this server needs build P16H6, 2026.10.08)'


def is_set_up(copy: pathlib.Path) -> bool:
    if WINDOWS:
        return (copy / 'sf-launch.exe').is_file()
    # Linux: the copy under /home/sfplay isn't readable by us, but sf-play (setup.sh) names the copy it starts.
    try:
        return f'{copy}/sf-launch.exe' in pathlib.Path('/usr/local/bin/sf-play').read_text()
    except OSError:
        return False


class App(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title('Soulframe private server')
        self.minsize(640, 420)
        self.server = None
        self.lines = queue.Queue()
        try:
            saved = json.loads(SETTINGS.read_text())
        except (OSError, ValueError):
            saved = {}
        self.folder = tk.StringVar(value=saved.get('game', ''))
        self.copy = tk.StringVar(value=saved.get('copy', str(DEFAULT_COPY)))
        self.host = tk.BooleanVar(value=saved.get('host', False))
        self.status = tk.StringVar()

        top = ttk.Frame(self, padding=8)
        top.pack(fill='x')
        ttk.Label(top, text='Your Soulframe folder (with Soulframe.x64.exe):').grid(row=0, column=0, columnspan=2, sticky='w')
        ttk.Entry(top, textvariable=self.folder).grid(row=1, column=0, sticky='ew', padx=(0, 6))
        ttk.Button(top, text='Browse...', command=self.browse).grid(row=1, column=1)
        ttk.Label(top, text=f'Put the private copy in (about 13 GB, a {COPY_NAME} folder of its own):').grid(
            row=2, column=0, columnspan=2, sticky='w', pady=(6, 0))
        ttk.Entry(top, textvariable=self.copy).grid(row=3, column=0, sticky='ew', padx=(0, 6))
        ttk.Button(top, text='Browse...', command=self.browse_copy).grid(row=3, column=1)
        ttk.Label(top, textvariable=self.status).grid(row=4, column=0, columnspan=2, sticky='w', pady=(4, 0))
        top.columnconfigure(0, weight=1)

        bar = ttk.Frame(self, padding=(8, 0))
        bar.pack(fill='x')
        self.setup_btn = ttk.Button(bar, text='1. Set up private copy', command=self.setup)
        self.server_btn = ttk.Button(bar, text='2. Start server', command=self.toggle_server)
        self.play_btn = ttk.Button(bar, text='3. Play', command=self.play)
        for b in self.setup_btn, self.server_btn, self.play_btn:
            b.pack(side='left', padx=(0, 6))
        ttk.Checkbutton(bar, text='Let other PCs join (--host)', variable=self.host).pack(side='left', padx=6)
        self.devgui = None
        self.dev = tk.BooleanVar(value=saved.get('dev', False))
        if WINDOWS:  # devgui.pyw drives the game window through user32; in game, F9 has the same panel
            ttk.Checkbutton(bar, text='Dev tools', variable=self.dev,
                            command=lambda: self.dev.get() and self.open_devgui()).pack(side='left', padx=6)
        else:  # no devgui on Linux: compile dev.lua into the copy once, and the F9 panel has the same buttons
            ttk.Button(bar, text='Load dev panel (F9)', command=self.load_dev_panel).pack(side='left', padx=6)

        self.log = tk.Text(self, height=16, state='disabled', wrap='none', font=('Consolas' if WINDOWS else 'monospace', 9))
        self.log.pack(fill='both', expand=True, padx=8, pady=8)
        self.say('Never log in with your real Digital Extremes account: use a made-up email and password.')
        self.check()
        self.after(100, self.pump)
        self.protocol('WM_DELETE_WINDOW', self.close)

    def say(self, line):
        self.log.configure(state='normal')
        self.log.insert('end', line.rstrip('\n') + '\n')
        self.log.see('end')
        self.log.configure(state='disabled')

    def pump(self):  # subprocess output arrives on reader threads; Tk may only be touched from this one
        while not self.lines.empty():
            self.say(self.lines.get())
        self.after(100, self.pump)

    def stream(self, proc, prefix=''):
        def read():
            for line in proc.stdout:
                self.lines.put(prefix + line)
            self.lines.put(f'{prefix}(exited with {proc.wait()})')
            self.after(0, self.check)
        threading.Thread(target=read, daemon=True).start()

    def save(self):
        SETTINGS.parent.mkdir(parents=True, exist_ok=True)
        SETTINGS.write_text(json.dumps({'game': self.folder.get(), 'copy': self.copy.get(), 'host': self.host.get(),
                                        'dev': self.dev.get()}))

    def copy_dir(self) -> pathlib.Path:
        return pathlib.Path(self.copy.get().strip())

    def open_devgui(self):
        self.save()
        if not (self.devgui and self.devgui.poll() is None):  # SF_COPY: where devlua.py writes dev.luac
            self.devgui = subprocess.Popen(script('--devgui'), cwd=DATA / 'tools', creationflags=NO_WINDOW,
                                           env={**ENV, 'SF_COPY': str(self.copy_dir())})

    def load_dev_panel(self):
        self.say('dev: compiling dev.lua into the copy; the game runs it once it is in the world and focused, then press F9')
        self.stream(subprocess.Popen(script('--devlua'), cwd=DATA / 'tools', stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                     stdin=subprocess.DEVNULL, text=True, env={**ENV, 'SF_COPY': str(self.copy_dir())}), 'dev: ')

    def browse_copy(self):
        picked = filedialog.askdirectory(title='Where to put the private copy', initialdir=self.copy_dir().parent)
        if picked:
            picked = pathlib.Path(picked)
            self.copy.set(str(picked if picked.name == COPY_NAME else picked / COPY_NAME))
            self.save()
            self.check()

    def browse(self):
        picked = filedialog.askdirectory(title='Soulframe folder (the one with Soulframe.x64.exe)', initialdir=self.folder.get() or None)
        if picked:
            self.folder.set(picked)
            self.save()
            self.check()

    def check(self):
        state = 'set up' if is_set_up(self.copy_dir()) else 'not set up yet'
        self.status.set(f'Private copy: {state}.  Server: {"running" if self.server and self.server.poll() is None else "stopped"}.')
        self.server_btn.configure(text='Stop server' if self.server and self.server.poll() is None else '2. Start server')

    def setup(self):
        folder = self.folder.get().strip()
        build = game_build(folder) if folder else 'pick your Soulframe folder first'
        if build != 'ok':
            return messagebox.showerror('Set up', build)
        copy = self.copy_dir()
        if copy.name != COPY_NAME or not copy.is_absolute():
            return messagebox.showerror('Set up', f'The private copy needs a folder of its own, named {COPY_NAME}.')
        self.save()
        if WINDOWS:  # setup.ps1 asks for admin itself and shows its progress in its own window
            subprocess.Popen(['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(JOIN / 'setup.ps1'),
                              '-Source', folder, '-Dest', str(copy)])
            self.say('Setup is running in its own window (accept the admin prompt). Click Play when it says Done.')
        else:  # our own prompt + sudo -S: pkexec needs a polkit agent, which many Arch desktops (tiling WMs) don't run
            password = simpledialog.askstring('Set up', 'Your password (setup runs with sudo):', show='*', parent=self)
            if password is None:
                return
            proc = subprocess.Popen(['sudo', '-S', '-k', '-p', '', 'bash', str(JOIN / 'setup.sh'), folder, str(copy)],
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.PIPE, text=True, env=ENV)
            proc.stdin.write(password + '\n')
            proc.stdin.close()
            self.stream(proc, 'setup: ')

    def toggle_server(self):
        if self.server and self.server.poll() is None:
            self.server.terminate()
            return
        self.save()
        self.server = subprocess.Popen(script('--server', *(['--host'] if self.host.get() else [])), cwd=DATA / 'tools', stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       stdin=subprocess.DEVNULL, text=True, errors='replace', creationflags=NO_WINDOW)
        self.stream(self.server)
        self.check()

    def play(self):
        copy = self.copy_dir()
        if not is_set_up(copy):
            return messagebox.showerror('Play', f'No private copy set up in {copy} yet: click 1. Set up private copy.')
        if not (self.server and self.server.poll() is None):
            self.toggle_server()
        # sf-launch.exe opens its own console if something is wrong; sf-play (Linux) prints to our log.
        if WINDOWS:
            subprocess.Popen([str(copy / 'sf-launch.exe')], cwd=copy)
            if self.dev.get():
                self.open_devgui()
        else:
            self.stream(subprocess.Popen(['sf-play'], cwd='/', stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                         stdin=subprocess.DEVNULL, text=True, env=ENV), 'game: ')

    def close(self):
        for proc in self.server, self.devgui:
            if proc and proc.poll() is None:
                proc.terminate()
        self.destroy()


if __name__ == '__main__':
    if FROZEN:
        unpack()
    if sys.argv[1:2] and sys.argv[1] in MODES:
        run_script(MODES[sys.argv[1]], sys.argv[2:])
    else:
        App().mainloop()
