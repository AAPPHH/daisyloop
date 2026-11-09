using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;
using System.Threading;
using LibreHardwareMonitor.Hardware;

namespace Daisyloop
{
    static class Program
    {
        static readonly Dictionary<SensorType, string> Units = new Dictionary<SensorType, string>
        {
            { SensorType.Voltage, "V" }, { SensorType.Current, "A" }, { SensorType.Power, "W" }, { SensorType.Clock, "MHz" },
            { SensorType.Temperature, "°C" }, { SensorType.Load, "%" }, { SensorType.Fan, "RPM" }, { SensorType.Flow, "L/h" },
            { SensorType.Control, "%" }, { SensorType.Level, "%" }, { SensorType.Factor, "" }, { SensorType.Data, "GB" },
            { SensorType.SmallData, "MB" }, { SensorType.Throughput, "B/s" }, { SensorType.Timing, "ns" }, { SensorType.Frequency, "Hz" },
            { SensorType.Noise, "dBA" }, { SensorType.Humidity, "%" }, { SensorType.Energy, "mWh" }, { SensorType.Conductivity, "uS/cm" },
        };

        sealed class Slot
        {
            public readonly object Lock = new object();
            public List<string> Items = new List<string>();
            public DateTime At = DateTime.MinValue;
        }

        static readonly List<Slot> Slots = new List<Slot>();
        static readonly object SlotsLock = new object();

        static void Esc(StringBuilder b, string s)
        {
            b.Append('"');
            foreach (char c in s ?? "")
            {
                if (c == '"' || c == '\\') b.Append('\\').Append(c);
                else if (c < 0x20) b.Append("\\u").Append(((int)c).ToString("x4"));
                else b.Append(c);
            }
            b.Append('"');
        }

        static void Visit(IHardware hw, string top, List<string> outp)
        {
            hw.Update();
            foreach (ISensor s in hw.Sensors)
            {
                if (!s.Value.HasValue || float.IsNaN(s.Value.Value) || float.IsInfinity(s.Value.Value)) continue;
                var b = new StringBuilder();
                string unit;
                if (!Units.TryGetValue(s.SensorType, out unit)) unit = "";
                b.Append("{\"id\":"); Esc(b, s.Identifier.ToString());
                b.Append(",\"hw\":"); Esc(b, top);
                b.Append(",\"name\":"); Esc(b, s.Name);
                b.Append(",\"type\":"); Esc(b, s.SensorType.ToString());
                b.Append(",\"unit\":"); Esc(b, unit);
                b.Append(",\"value\":").Append(((double)s.Value.Value).ToString("R", CultureInfo.InvariantCulture));
                outp.Add(b.ToString());
            }
            foreach (IHardware sub in hw.SubHardware) Visit(sub, top, outp);
        }

        static void SlowGroup(int periodMs)
        {
            Computer slow;
            try
            {
                slow = new Computer { IsMemoryEnabled = true, IsStorageEnabled = true };
                slow.Open();
            }
            catch (Exception e) { Console.Error.WriteLine("slow open: " + e.Message); return; }
            foreach (IHardware hw in slow.Hardware)
            {
                var slot = new Slot();
                lock (SlotsLock) Slots.Add(slot);
                IHardware h = hw;
                var t = new Thread(() =>
                {
                    while (true)
                    {
                        try
                        {
                            var items = new List<string>();
                            Visit(h, h.Name, items);
                            lock (slot.Lock) { slot.Items = items; slot.At = DateTime.UtcNow; }
                        }
                        catch (Exception e) { Console.Error.WriteLine("slow update " + h.Name + ": " + e.Message); }
                        Thread.Sleep(periodMs);
                    }
                }) { IsBackground = true, Name = "slow-" + h.Name };
                t.Start();
            }
        }

        static int Main(string[] args)
        {
            CultureInfo.DefaultThreadCurrentCulture = CultureInfo.InvariantCulture;
            Thread.CurrentThread.CurrentCulture = CultureInfo.InvariantCulture;
            int periodMs = args.Length > 0 ? int.Parse(args[0], CultureInfo.InvariantCulture) : 1000;
            int slowMs = args.Length > 1 ? int.Parse(args[1], CultureInfo.InvariantCulture) : 10000;
            var computer = new Computer
            {
                IsCpuEnabled = true, IsGpuEnabled = true, IsMotherboardEnabled = true,
                IsControllerEnabled = true, IsNetworkEnabled = true, IsPsuEnabled = true, IsBatteryEnabled = true,
            };
            computer.Open();
            new Thread(() => SlowGroup(slowMs)) { IsBackground = true, Name = "slow-open" }.Start();
            var stdout = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true, NewLine = "\n" };
            long seq = 0;
            while (true)
            {
                var started = DateTime.UtcNow;
                var items = new List<string>();
                foreach (IHardware hw in computer.Hardware) Visit(hw, hw.Name, items);
                var b = new StringBuilder();
                b.Append("{\"seq\":").Append(++seq).Append(",\"sensors\":[");
                bool first = true;
                foreach (string it in items)
                {
                    if (!first) b.Append(',');
                    first = false;
                    b.Append(it).Append('}');
                }
                Slot[] slots;
                lock (SlotsLock) slots = Slots.ToArray();
                var now = DateTime.UtcNow;
                foreach (Slot slot in slots)
                {
                    List<string> its; DateTime at;
                    lock (slot.Lock) { its = slot.Items; at = slot.At; }
                    if (at == DateTime.MinValue) continue;
                    string age = (now - at).TotalSeconds.ToString("F1", CultureInfo.InvariantCulture);
                    foreach (string it in its)
                    {
                        if (!first) b.Append(',');
                        first = false;
                        b.Append(it).Append(",\"age\":").Append(age).Append('}');
                    }
                }
                b.Append("]}");
                try { stdout.WriteLine(b.ToString()); } catch (IOException) { return 0; }
                int left = periodMs - (int)(DateTime.UtcNow - started).TotalMilliseconds;
                if (left > 0) Thread.Sleep(left);
            }
        }
    }
}
