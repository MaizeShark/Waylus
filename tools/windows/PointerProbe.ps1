# Pointer listener for checking Waylus's Windows pen/touch injection (waylus --test-input).
# With -Stream, prints "DOWN <QueryPerformanceCounter ticks> <frequency>" for every pen-down
# as it arrives (used by tools/latency/pen.py on Windows). Opens a borderless window on one monitor, records WM_POINTER* messages (pen pressure,
# tilt, flags; touch contacts) and writes a summary to -Out when it closes.
#
#   powershell -ExecutionPolicy Bypass -File tools\windows\PointerProbe.ps1 -Monitor 2 -Seconds 12 -Out probe.txt
param([int]$Monitor = 2, [int]$Seconds = 12, [string]$Out = "pointer-probe.txt", [switch]$Stream)

Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows.Forms;

public class ProbeForm : Form {
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [DllImport("user32.dll")] static extern bool GetPointerType(uint id, out int type);
    [DllImport("user32.dll")] static extern bool GetPointerPenInfo(uint id, out PEN_INFO info);
    [DllImport("user32.dll")] static extern bool GetPointerTouchInfo(uint id, out TOUCH_INFO info);

    [StructLayout(LayoutKind.Sequential)] struct PT { public int x, y; }
    [StructLayout(LayoutKind.Sequential)] struct RC { public int l, t, r, b; }
    [StructLayout(LayoutKind.Sequential)] struct PINFO {
        public int type; public uint id, frame, flags; public IntPtr src, hwnd; public PT px, hm, pxr, hmr;
        public uint time, history; public int inputData; public uint keyStates; public ulong perf; public int btn;
    }
    [StructLayout(LayoutKind.Sequential)] struct PEN_INFO { public PINFO p; public uint penFlags, penMask, pressure, rotation; public int tiltX, tiltY; }
    [StructLayout(LayoutKind.Sequential)] struct TOUCH_INFO { public PINFO p; public uint touchFlags, touchMask; public RC c, cr; public uint orientation, pressure; }

    public bool stream; public int penEvents, penContact, penHover, penBarrel, penEraser, penDown, penUp, touchEvents, touchDown, touchUp;
    public uint pMin = 99999, pMax = 0; public int tMinX = 999, tMaxX = -999, tMinY = 999, tMaxY = -999;
    public HashSet<uint> touchIds = new HashSet<uint>();
    public int mouseMoves, mouseDowns;
    public int minX = int.MaxValue, maxX = int.MinValue; public List<string> first = new List<string>(); public SortedSet<string> combos = new SortedSet<string>();
    public int touchMaxConcurrent; HashSet<uint> live = new HashSet<uint>();

    public ProbeForm(Rectangle r, int seconds) {
        FormBorderStyle = FormBorderStyle.None; StartPosition = FormStartPosition.Manual;
        Bounds = r; TopMost = true; BackColor = Color.Black; ShowInTaskbar = false;
        var t = new Timer { Interval = seconds * 1000 }; t.Tick += (s, e) => Close(); t.Start();
    }
    protected override void WndProc(ref Message m) {
        int msg = m.Msg;
        if (msg >= 0x245 && msg <= 0x24A) {
            uint id = (uint)(m.WParam.ToInt64() & 0xffff);
            int type; GetPointerType(id, out type);
            if (type == 3) {            // PT_PEN
                PEN_INFO pi;
                if (GetPointerPenInfo(id, out pi)) {
                    penEvents++;
                    uint f = pi.p.flags;
                    bool contact = (f & 0x4) != 0;
                    if (contact) { penContact++; if (pi.pressure < pMin) pMin = pi.pressure; if (pi.pressure > pMax) pMax = pi.pressure; }
                    else penHover++;
                    if ((pi.penFlags & 1) != 0) penBarrel++;
                    if ((pi.penFlags & 4) != 0 || (pi.penFlags & 2) != 0) penEraser++;
                    combos.Add(String.Format("msg=0x{0:x} flags=0x{1:x} penFlags=0x{2:x} buttonChange={3}", msg, f & 0xfff, pi.penFlags, pi.p.btn));
                    if (msg == 0x246) { penDown++; if (stream) { Console.Out.WriteLine("DOWN " + System.Diagnostics.Stopwatch.GetTimestamp() + " " + System.Diagnostics.Stopwatch.Frequency); Console.Out.Flush(); } }
                    if (msg == 0x247) penUp++;
                    if (pi.tiltX < tMinX) tMinX = pi.tiltX; if (pi.tiltX > tMaxX) tMaxX = pi.tiltX;
                    if (pi.tiltY < tMinY) tMinY = pi.tiltY; if (pi.tiltY > tMaxY) tMaxY = pi.tiltY;
                    if (pi.p.px.x < minX) minX = pi.p.px.x; if (pi.p.px.x > maxX) maxX = pi.p.px.x;
                    if (first.Count < 6 || (contact && first.Count < 12))
                        first.Add(String.Format("msg=0x{0:x} flags=0x{1:x} pen=0x{2:x} p={3} tilt={4},{5} at {6},{7}", msg, f, pi.penFlags, pi.pressure, pi.tiltX, pi.tiltY, pi.p.px.x, pi.p.px.y));
                }
            } else if (type == 2) {     // PT_TOUCH
                TOUCH_INFO ti;
                if (GetPointerTouchInfo(id, out ti)) {
                    touchEvents++; touchIds.Add(id);
                    if (msg == 0x246) { touchDown++; live.Add(id); if (live.Count > touchMaxConcurrent) touchMaxConcurrent = live.Count; }
                    if (msg == 0x247) { touchUp++; live.Remove(id); }
                }
            }
        }
        base.WndProc(ref m);
    }
    protected override void OnMouseMove(MouseEventArgs e) { mouseMoves++; base.OnMouseMove(e); }
    protected override void OnMouseDown(MouseEventArgs e) { mouseDowns++; base.OnMouseDown(e); }
}
'@

[ProbeForm]::new([System.Drawing.Rectangle]::Empty, 1) | Out-Null  # compile check only
$null = Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);' -Name Dpi -Namespace W -PassThru
[W.Dpi]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
# Same numbering as `waylus --list-outputs`: by device number (\\.\DISPLAYn).
$screens = [System.Windows.Forms.Screen]::AllScreens | Sort-Object { [int]($_.DeviceName -replace '\D', '') }
$r = $screens[$Monitor - 1].Bounds
$f = [ProbeForm]::new($r, $Seconds)
$f.stream = [bool]$Stream
if ($Stream) { Write-Output "READY" }
[System.Windows.Forms.Application]::Run($f)
$s = @()
$s += "monitor rect: $($r.X),$($r.Y) $($r.Width)x$($r.Height)"
$s += "pen events $($f.penEvents): contact $($f.penContact), hover $($f.penHover), downs $($f.penDown), ups $($f.penUp), barrel $($f.penBarrel), eraser $($f.penEraser)"
$s += "pen pressure range $($f.pMin)..$($f.pMax) (0..1024)"
$s += "tilt x $($f.tMinX)..$($f.tMaxX), y $($f.tMinY)..$($f.tMaxY)"
$s += "pen x range $($f.minX)..$($f.maxX)"
$s += "touch events $($f.touchEvents), ids $($f.touchIds.Count), downs $($f.touchDown), ups $($f.touchUp), max concurrent $($f.touchMaxConcurrent)"
$s += "mouse moves $($f.mouseMoves), downs $($f.mouseDowns)"
$s += "distinct (message, pointer flags, pen flags):"
$s += $f.combos
$s += $f.first
$s | Set-Content -Encoding utf8 $Out
$s
