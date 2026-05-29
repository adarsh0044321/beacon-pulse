using System;
using System.IO;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;

class PulseSetup {
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
    Console.Title = "Pulse Player Setup v1.0.3";
    Console.ForegroundColor = ConsoleColor.Cyan;
    Console.WriteLine();
    Console.WriteLine("  ╔══════════════════════════════════════╗");
    Console.WriteLine("  ║     Pulse Player Setup  v1.0.3       ║");
    Console.WriteLine("  ║     LAN Screen Sharing Receiver      ║");
    Console.WriteLine("  ╚══════════════════════════════════════╝");
    Console.ResetColor(); Console.WriteLine();
  }

  static void Main() {
    Header();

    string dir = Path.Combine(
      Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
      "PulsePlayer");

    bool alreadyInstalled = Directory.Exists(dir) &&
      File.Exists(Path.Combine(dir, "pulse.exe"));

    bool isReinstall = false;

    if (alreadyInstalled) {
      Console.ForegroundColor = ConsoleColor.White;
      Console.WriteLine("  Pulse Player is already installed on this PC.");
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
    Msg("Stopping existing Pulse processes...");
    try {
      foreach (var p in Process.GetProcessesByName("pulse")) {
        p.Kill(); p.WaitForExit(3000);
      }
    } catch { }
    System.Threading.Thread.Sleep(800);

    // ── Step 2: Clean up old files (reinstall) ──────────────────────────
    if (isReinstall) {
      Msg("Removing old installation...");

      // Remove all files in the install directory
      try {
        foreach (var file in Directory.GetFiles(dir)) {
          try { File.Delete(file); } catch { }
        }
        foreach (var subdir in Directory.GetDirectories(dir)) {
          try { Directory.Delete(subdir, true); } catch { }
        }
      } catch { }

      // Remove desktop shortcut
      string oldLnk = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.Desktop),
        "Pulse Player.lnk");
      try { if (File.Exists(oldLnk)) File.Delete(oldLnk); } catch { }

      // Remove firewall rules (clean slate)
      foreach (var rn in new[] { "Pulse-UDP-ClientRecv", "Beacon-Pulse-UDP-Discovery" }) {
        try {
          var pi = new ProcessStartInfo("netsh") {
            Arguments = "advfirewall firewall delete rule name=" + rn,
            UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true
          };
          var _p = Process.Start(pi); if (_p != null) _p.WaitForExit(3000);
        } catch { }
      }

      Console.ForegroundColor = ConsoleColor.Green;
      Console.WriteLine("    Old files removed.");
      Console.ResetColor();
    }

    // ── Step 3: Create install folder ────────────────────────────────────
    Msg("Creating install folder: %LOCALAPPDATA%\\PulsePlayer");
    Directory.CreateDirectory(dir);

    // ── Step 4: Extract binaries ─────────────────────────────────────────
    Msg("Writing pulse.exe ...");
    File.WriteAllBytes(Path.Combine(dir, "pulse.exe"), GetRes("pulse.exe"));

    // ── Step 4.5: Firewall rules ──────────────────────────────────────────
    Msg("Configuring Windows Firewall...");
    string[][] rules = {
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

    // ── Step 5: Desktop shortcut ─────────────────────────────────────────
    Msg("Creating Desktop shortcut...");
    string uiExe = Path.Combine(dir, "pulse.exe");
    string lnk = Path.Combine(
      Environment.GetFolderPath(Environment.SpecialFolder.Desktop),
      "Pulse Player.lnk");
    string ps = "$s=(New-Object -COM WScript.Shell).CreateShortcut('" + lnk + "');"
      + "$s.TargetPath='" + uiExe + "';$s.WorkingDirectory='" + dir + "';"
      + "$s.Description='Pulse - LAN Screen Sharing Receiver';$s.Save()";
    try {
      var _q = Process.Start(new ProcessStartInfo("powershell") {
        Arguments = "-NoProfile -ExecutionPolicy Bypass -Command \"" + ps + "\"",
        UseShellExecute = false, CreateNoWindow = true
      });
      if (_q != null) _q.WaitForExit(5000);
    } catch { }

    // ── Step 6: Start Menu Shortcut ──────────────────────────────────────
    Msg("Creating Start Menu shortcut...");
    string smDir = Path.Combine(
      Environment.GetFolderPath(Environment.SpecialFolder.StartMenu),
      "Programs\\Pulse");
    Directory.CreateDirectory(smDir);
    string smLnk = Path.Combine(smDir, "Pulse Player.lnk");
    string smPs = "$s=(New-Object -COM WScript.Shell).CreateShortcut('" + smLnk + "');"
      + "$s.TargetPath='" + uiExe + "';$s.WorkingDirectory='" + dir + "';"
      + "$s.Description='Pulse - LAN Screen Sharing Receiver';$s.Save()";
    try {
      var _q = Process.Start(new ProcessStartInfo("powershell") {
        Arguments = "-NoProfile -ExecutionPolicy Bypass -Command \"" + smPs + "\"",
        UseShellExecute = false, CreateNoWindow = true
      });
      if (_q != null) _q.WaitForExit(5000);
    } catch { }

    // ── Done ─────────────────────────────────────────────────────────────
    Console.WriteLine();
    Console.ForegroundColor = ConsoleColor.Green;
    string label = isReinstall ? "Reinstall" : "Installation";
    Console.WriteLine("  ╔══════════════════════════════════════╗");
    Console.WriteLine("  ║  " + label.PadRight(35) + "║");
    Console.WriteLine("  ║  complete!                          ║");
    Console.WriteLine("  ║  Desktop shortcut: Pulse Player     ║");
    Console.WriteLine("  ╚══════════════════════════════════════╝");
    Console.ResetColor(); Console.WriteLine();
    Console.WriteLine("  Press any key to close...");
    Console.ReadKey(true);
  }
}
