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

        static void Visit(IHardware hw, string top, StringBuilder b, ref bool first)
        {
            hw.Update();
            foreach (ISensor s in hw.Sensors)
            {
                if (!s.Value.HasValue || float.IsNaN(s.Value.Value) || float.IsInfinity(s.Value.Value)) continue;
                if (!first) b.Append(',');
                first = false;
                string unit;
                if (!Units.TryGetValue(s.SensorType, out unit)) unit = "";
                b.Append("{\"id\":"); Esc(b, s.Identifier.ToString());
                b.Append(",\"hw\":"); Esc(b, top);
                b.Append(",\"name\":"); Esc(b, s.Name);
                b.Append(",\"type\":"); Esc(b, s.SensorType.ToString());
                b.Append(",\"unit\":"); Esc(b, unit);
                b.Append(",\"value\":").Append(((double)s.Value.Value).ToString("R", CultureInfo.InvariantCulture));
                b.Append('}');
            }
            foreach (IHardware sub in hw.SubHardware) Visit(sub, top, b, ref first);
        }

        static int Main(string[] args)
        {
            CultureInfo.DefaultThreadCurrentCulture = CultureInfo.InvariantCulture;
            Thread.CurrentThread.CurrentCulture = CultureInfo.InvariantCulture;
            int periodMs = args.Length > 0 ? int.Parse(args[0], CultureInfo.InvariantCulture) : 1000;
            var computer = new Computer
            {
                IsCpuEnabled = true, IsGpuEnabled = true, IsMemoryEnabled = true, IsMotherboardEnabled = true,
                IsControllerEnabled = true, IsNetworkEnabled = true, IsStorageEnabled = true, IsPsuEnabled = true, IsBatteryEnabled = true,
            };
            computer.Open();
            var stdout = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true, NewLine = "\n" };
            long seq = 0;
            while (true)
            {
                var started = DateTime.UtcNow;
                var b = new StringBuilder();
                b.Append("{\"seq\":").Append(++seq).Append(",\"sensors\":[");
                bool first = true;
                foreach (IHardware hw in computer.Hardware) Visit(hw, hw.Name, b, ref first);
                b.Append("]}");
                try { stdout.WriteLine(b.ToString()); } catch (IOException) { return 0; }
                int left = periodMs - (int)(DateTime.UtcNow - started).TotalMilliseconds;
                if (left > 0) Thread.Sleep(left);
            }
        }
    }
}
