param([int]$Seconds = 20)
Add-Type -Language CSharp @'
using System;
using System.Runtime.InteropServices;

[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] public class MMDeviceEnumerator { }

[ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDeviceEnumerator {
  int EnumAudioEndpoints(int dataFlow, int stateMask, out IntPtr devices);
  int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice device);
}

[ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDevice {
  int Activate(ref Guid iid, int clsCtx, IntPtr act, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
  int OpenPropertyStore(int access, out IntPtr store);
  int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
  int GetState(out int state);
}

[ComImport, Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioMeterInformation {
  int GetPeakValue(out float peak);
}

[ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioSessionManager2 {
  int NotUsed1(); int NotUsed2();
  int GetSessionEnumerator(out IAudioSessionEnumerator e);
}

[ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioSessionEnumerator {
  int GetCount(out int count);
  int GetSession(int index, out IAudioSessionControl s);
}

[ComImport, Guid("F4B1A599-7266-4319-A8CA-E70ACB11E8CD"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioSessionControl {
  int GetState(out int state);
  int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string name);
}

public static class Audio {
  static IMMDevice dev;
  static IAudioMeterInformation meter;
  static IAudioSessionManager2 mgr;
  public static string Init() {
    var e = (IMMDeviceEnumerator)(new MMDeviceEnumerator());
    e.GetDefaultAudioEndpoint(0, 0, out dev);
    Guid m = new Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064");
    object o; dev.Activate(ref m, 1, IntPtr.Zero, out o); meter = (IAudioMeterInformation)o;
    Guid s = new Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F");
    object o2; dev.Activate(ref s, 1, IntPtr.Zero, out o2); mgr = (IAudioSessionManager2)o2;
    string id; dev.GetId(out id); return id;
  }
  public static float Peak() { float p; meter.GetPeakValue(out p); return p; }
  public static string Sessions() {
    IAudioSessionEnumerator en; mgr.GetSessionEnumerator(out en);
    int n; en.GetCount(out n); int active = 0;
    for (int i = 0; i < n; i++) {
      IAudioSessionControl c; en.GetSession(i, out c);
      int st; c.GetState(out st); if (st == 1) active++;
    }
    return n + "/" + active;
  }
}
'@
"endpoint " + [Audio]::Init()
for ($i = 0; $i -lt $Seconds; $i++) {
  "t=$i peak=$([Audio]::Peak()) sessions(total/active)=$([Audio]::Sessions())"
  Start-Sleep -Milliseconds 1000
}
