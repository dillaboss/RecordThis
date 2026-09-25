# RecordThis

A Windows 11 audio recorder. It records either a microphone or **everything a playback device plays** (WASAPI loopback), writes audio straight to disk as it records, then lets you preview and save a standard WAV file.

> **Status: untested.** This code was written without access to a Windows machine or the .NET SDK, so it has never been compiled or run. Expect a few build errors (most likely in XAML bindings) and treat all performance figures in the spec as unverified.

## Requirements

- Windows 11 (build 22000 or later), x64 or ARM64
- Visual Studio 2022 17.8+ with the **.NET desktop development** workload and the **Windows App SDK C# Templates** component, or the .NET 8 SDK on its own
- Windows 10/11 SDK 10.0.22621 or later

## Build and run

From the project folder:

```powershell
# Debug build (x64)
dotnet build -p:Platform=x64

# Run
dotnet run -p:Platform=x64

# ARM64
dotnet build -p:Platform=ARM64
```

In Visual Studio, open `RecordThis.csproj`, pick **x64** (or ARM64) in the platform dropdown, and press F5. The app is unpackaged (`WindowsPackageType=None`), so no MSIX deployment step is needed.

## Publish a single self-contained .exe

```powershell
dotnet publish -c Release -r win-x64 -p:Platform=x64
dotnet publish -c Release -r win-arm64 -p:Platform=ARM64
```

Output lands in `bin\Release\net8.0-windows10.0.22621.0\<rid>\publish\`. The Windows App SDK runtime and .NET runtime are bundled, so the target PC needs nothing installed.

If the single-file build fails to start (WinUI 3 single-file support has historically been fragile), publish as a folder instead:

```powershell
dotnet publish -c Release -r win-x64 -p:Platform=x64 -p:PublishSingleFile=false
```

## Build online with GitHub Actions (no Visual Studio)

The project includes `.github/workflows/build.yml`, which builds RecordThis on a GitHub-hosted Windows machine.

1. Create a new repository on github.com and upload the contents of this folder to it (including the `.github` folder).
2. Open the **Actions** tab. A build starts automatically on every upload; you can also start one with **Run workflow**.
3. When it finishes (about 5–10 minutes), open the run and download an artifact:
   - `RecordThis-win-x64`: the single-file EXE for most PCs
   - `RecordThis-win-x64-folder`: the fallback folder build, if the single EXE won't start
   - `-win-arm64` versions for ARM PCs (e.g. Snapdragon laptops)

The EXE is unsigned, so Windows SmartScreen will warn on first launch: choose **More info → Run anyway**.

## Using it

1. Pick a source. Speakers and headphones record everything the PC plays (loopback); microphones record input.
2. Press **Start recording**. The level meter and timer run while recording.
3. Press **Stop recording**. The Review screen shows the waveform; click or drag it to scrub, or use the arrow keys.
4. Choose a sample rate (44.1 or 48 kHz) and bit depth (16-bit dithered or 24-bit), then **Save As (.wav)**.

If RecordThis crashes, loses power or is force-closed mid-recording, the next launch offers to recover the audio.

## How the spec maps to the code

| Spec component | File |
|---|---|
| WASAPI capture / loopback, silence gap watchdog | `Services/Audio/WasapiAudioCaptureService.cs` |
| Zero-frame generator | `Services/Audio/SilenceInjectionBuffer.cs` |
| ITU-R BS.775 downmix coefficients | `Services/Audio/DownmixMatrix.cs`, `Pipeline/DownmixSampleProvider.cs` |
| WdlResampler to 44.1/48 kHz | `Pipeline/ExactLengthResampler.cs` |
| 16/24-bit PCM + RIFF writer | `Services/Audio/AudioExportService.cs` |
| Device enumeration, hot-plug (`IMMNotificationClient`) | `Services/Devices/WindowsDeviceManager.cs` |
| Disk spool + crash recovery | `Services/Storage/SpoolFileManager.cs` |
| `PowerModeChanged` finalize-before-sleep | `Services/Platform/PowerEventService.cs` |
| WASAPI error → user guidance (exclusive mode, privacy, etc.) | `Services/Audio/AudioErrorTranslator.cs` |
| Capture view / view model | `Views/CaptureView.xaml`, `ViewModels/CaptureViewModel.cs` |
| Review view / view model | `Views/ReviewView.xaml`, `Views/WaveformView.xaml`, `ViewModels/ReviewViewModel.cs` |
| Mica Alt window, title bar, close protection | `MainWindow.xaml(.cs)` |

### Recording pipeline

```
WASAPI packet ──► spool file (%LOCALAPPDATA%\Temp\RecordThis_{guid}.raw, native mix format)
     ▲                    + RecordThis_{guid}.json (format sidecar for recovery)
     └─ watchdog: no packet for 1.5× buffer (150 ms) → write zero frames for the gap
```

### Export pipeline (all streamed from disk)

```
spool → float decode → BS.775 downmix (if >2 ch) → clip-safe gain → WdlResampler (sinc)
      → 16-bit TPDF dither or 24-bit → WAV (.partial, renamed when complete)
```

## Deviations from the spec

These are deliberate, and each one is worth knowing about:

- **Binary size and AOT.** The spec's under-28 MB Native AOT target isn't achievable here. NAudio's WASAPI layer relies on classic COM interop, which trimming and AOT break, so both are off. A self-contained WinUI 3 single-file build will be substantially larger than 28 MB.
- **Level meter bug fixed.** The spec's sample `ComputePeak` checks `WaveFormatEncoding.IeeeFloat`, but WASAPI almost always reports `Extensible`, so the meter would sit at zero. `CaptureFormat.FromWaveFormat` reads the real sub-format from `WAVEFORMATEXTENSIBLE`.
- **Downmix clipping.** The BS.775 coefficients sum above 1.0, so loud surround material can clip after fold-down. RecordThis measures the post-downmix peak and, only if it exceeds 0 dBFS, lowers the whole file to peak at −0.1 dBFS. The Review screen reports the reduction.
- **Streaming preview.** The spec loads the recording into memory for review. RecordThis streams it from the spool instead, which keeps memory flat for long recordings.
- **Watchdog precision.** Silence is inserted within roughly one 20 ms timer tick, not with literal zero drift. The anchor resets on every real packet, so error doesn't accumulate.
- **Meter only while recording.** The live meter runs during capture, not while idle.
- **Playback fallback.** If WASAPI shared mode rejects the preview stream's format, playback falls back to `WaveOutEvent` (adds the `NAudio.WinMM` package).
- **Sleep on Modern Standby devices.** `PowerModeChanged` is reliable for classic S3 sleep and hibernate. On some Modern Standby (S0ix) laptops the notification may arrive late or not at all; crash recovery covers that case.
- **WAV size limit.** WAV files max out at 4 GB (about 6 hours of 48 kHz/24-bit stereo). Longer recordings are refused at export with a message suggesting a smaller format.

## Troubleshooting

- **"Another app has exclusive control of this device."** Close the other app, or in Sound settings open the device's **Properties → Advanced** and clear *Allow applications to take exclusive control of this device*.
- **"Windows is blocking microphone access."** Turn on *Let desktop apps access your microphone* under **Settings → Privacy & security → Microphone**.
- **Package restore fails.** The package versions in `RecordThis.csproj` may be outdated. Update them to the latest stable Windows App SDK, CommunityToolkit.Mvvm and NAudio 2.x releases.
