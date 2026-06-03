using System;
using System.IO;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;

class BeaconSetup {
  [DllImport("kernel32.dll")] static extern bool SetFileAttributesW(string f, uint a);

  static byte[] GetRes(string name) {
    var asm = Assembly.GetExecutingAssembly();
    using (var s = asm.GetManifestResourceStream(name)) {
      if (s == null) throw new Exception("Resource not found: " + name);
      var buf = new byte[s.Length]; s.Read(buf, 0, (int)s.Length); return buf;
    }
  }

  static void Msg(string m) {
    Console.Write("  "); Console.ForegroundColor = ConsoleColor.Yellow;
    Console.Write("> "); Console.ResetColor(); Console.WriteLine(m);
  }

  static void Header() {
    Console.Clear();
    Console.Title = "Beacon Setup v1.0.6";
    Console.ForegroundColor = ConsoleColor.Cyan;
    Console.WriteLine();
    Console.WriteLine("  ╔══════════════════════════════════════╗");
    Console.WriteLine("  ║     Beacon Setup  v1.0.6             ║");
    Console.WriteLine("  ║     LAN Screen Sharing               ║");
    Console.WriteLine("  ╚══════════════════════════════════════╝");
    Console.ResetColor(); Console.WriteLine();
  }

  static void Main() {
    Header();

    string dir = Path.Combine(
      Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
      ".beacon");

    bool alreadyInstalled = Directory.Exists(dir) &&
      File.Exists(Path.Combine(dir, "beacon.exe"));

    bool isReinstall = false;

    if (alreadyInstalled) {
      Console.ForegroundColor = ConsoleColor.White;
      Console.WriteLine("  Beacon is already installed on this PC.");
      Console.WriteLine();
      Console.ForegroundColor = ConsoleColor.Cyan;
      Console.WriteLine("  Choose an option:");
      Console.WriteLine();
      Console.ForegroundColor = ConsoleColor.White;
      Console.WriteLine("    [1]  Reinstall  — removes all old files, installs fresh");
      Console.WriteLine("    [2]  Cancel     — exit without changes");
      Console.WriteLine();
      Console.ResetColor();
      Console.Write("  Enter choice (1/2): ");

      while (true) {
        var key = Console.ReadKey(true);
        if (key.KeyChar == '1') {
          Console.WriteLine("1");
          isReinstall = true;
          break;
        }
        if (key.KeyChar == '2' || key.Key == ConsoleKey.Escape) {
          Console.WriteLine("2");
          Console.WriteLine();
          Console.ForegroundColor = ConsoleColor.Yellow;
          Console.WriteLine("  Installation cancelled.");
          Console.ResetColor();
          Console.WriteLine("  Press any key to close...");
          Console.ReadKey(true);
          return;
        }
      }
    } else {
      Console.ForegroundColor = ConsoleColor.White;
      Console.WriteLine("  Fresh installation — no previous install detected.");
      Console.ResetColor();
    }

    Console.WriteLine();

    // ── Step 1: Kill existing processes ──────────────────────────────────
    Msg("Stopping existing Beacon processes...");
    foreach (var n in new[] { "beacon-watchdog", "beacon" }) {
      try {
        foreach (var p in Process.GetProcessesByName(n)) {
          p.Kill(); p.WaitForExit(3000);
        }
      } catch { }
    }
    System.Threading.Thread.Sleep(800);

    // ── Step 2: Clean up old files (reinstall) ──────────────────────────
    if (isReinstall) {
      Msg("Removing old installation...");

      // Remove all files in the install directory
      try {
        foreach (var file in Directory.GetFiles(dir)) {
          try { File.Delete(file); } catch { }
        }
        // Also remove logs and any sub-dirs
        foreach (var subdir in Directory.GetDirectories(dir)) {
          try { Directory.Delete(subdir, true); } catch { }
        }
      } catch { }

      // Remove desktop shortcut
      string oldLnk = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.Desktop),
        "Beacon.lnk");
      try { if (File.Exists(oldLnk)) File.Delete(oldLnk); } catch { }

      // Remove firewall rules (clean slate)
      foreach (var rn in new[] {
        "Beacon-UDP-Stream", "Beacon-TCP-Control",
        "Pulse-UDP-ClientRecv", "Beacon-Pulse-UDP-Discovery" }) {
        try {
          var pi = new ProcessStartInfo("netsh") {
            Arguments = "advfirewall firewall delete rule name=" + rn,
            UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true
          };
          var _p = Process.Start(pi); if (_p != null) _p.WaitForExit(3000);
        } catch { }
      }

      // Also remove logs dir
      string logsDir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Beacon");
      try { if (Directory.Exists(logsDir)) Directory.Delete(logsDir, true); } catch { }

      Console.ForegroundColor = ConsoleColor.Green;
      Console.WriteLine("    Old files removed.");
      Console.ResetColor();
    }

    // ── Step 3: Create install folder ────────────────────────────────────
    Msg("Creating install folder: %LOCALAPPDATA%\\.beacon (hidden)");
    Directory.CreateDirectory(dir);
    SetFileAttributesW(dir, 2); // FILE_ATTRIBUTE_HIDDEN

    // ── Step 4: Extract binaries ─────────────────────────────────────────
    Msg("Writing beacon.exe ...");
    File.WriteAllBytes(Path.Combine(dir, "beacon.exe"), GetRes("beacon.exe"));
    Msg("Writing beacon-watchdog.exe ...");
    File.WriteAllBytes(Path.Combine(dir, "beacon-watchdog.exe"), GetRes("beacon-watchdog.exe"));

    // ── Step 5: Firewall rules ───────────────────────────────────────────
    Msg("Configuring Windows Firewall...");
    string[][] rules = {
      new[] { "Beacon-UDP-Stream",         "UDP", "45100" },
      new[] { "Beacon-TCP-Control",        "TCP", "45101" },
      new[] { "Pulse-UDP-ClientRecv",      "UDP", "45102" },
      new[] { "Beacon-Pulse-UDP-Discovery", "UDP", "45199" }
    };
    foreach (var r in rules) {
      try {
        var pi = new ProcessStartInfo("netsh") {
          Arguments = "advfirewall firewall add rule name=" + r[0] +
            " dir=in action=allow protocol=" + r[1] + " localport=" + r[2] +
            " enable=yes profile=any",
          UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true
        };
        var _p = Process.Start(pi); if (_p != null) _p.WaitForExit(5000);
      } catch { }
    }

    // ── Step 6: Desktop shortcut ─────────────────────────────────────────
    Msg("Creating Desktop shortcut...");
    string uiExe = Path.Combine(dir, "beacon.exe");
    string lnk = Path.Combine(
      Environment.GetFolderPath(Environment.SpecialFolder.Desktop),
      "Beacon.lnk");
    string ps = "$s=(New-Object -COM WScript.Shell).CreateShortcut('" + lnk + "');"
      + "$s.TargetPath='" + uiExe + "';$s.WorkingDirectory='" + dir + "';"
      + "$s.Description='Beacon - LAN Screen Sharing';$s.Save()";
    try {
      var _q = Process.Start(new ProcessStartInfo("powershell") {
        Arguments = "-NoProfile -ExecutionPolicy Bypass -Command \"" + ps + "\"",
        UseShellExecute = false, CreateNoWindow = true
      });
      if (_q != null) _q.WaitForExit(5000);
    } catch { }

    // ── Step 7: Launch ───────────────────────────────────────────────────
    Msg("Starting Beacon...");
    try {
      Process.Start(new ProcessStartInfo(Path.Combine(dir, "beacon-watchdog.exe")) {
        WorkingDirectory = dir, UseShellExecute = false, CreateNoWindow = true
      });
    } catch { }

    // ── Done ─────────────────────────────────────────────────────────────
    Console.WriteLine();
    Console.ForegroundColor = ConsoleColor.Green;
    string label = isReinstall ? "Reinstall" : "Installation";
    Console.WriteLine("  ╔══════════════════════════════════════╗");
    Console.WriteLine("  ║  " + label.PadRight(35) + "║");
    Console.WriteLine("  ║  complete!                          ║");
    Console.WriteLine("  ║  Desktop shortcut: Beacon           ║");
    Console.WriteLine("  ╚══════════════════════════════════════╝");
    Console.ResetColor(); Console.WriteLine();
    Console.WriteLine("  Press any key to close...");
    Console.ReadKey(true);
  }
}
