namespace CodexBar.App.Tests;

using System.Runtime.ExceptionServices;
using System.Threading;
using System.Windows;
using System.Windows.Media;
using System.Windows.Threading;
using CodexBar.Core.Configuration;
using Microsoft.Extensions.Logging.Abstractions;

public sealed class ClampToWorkAreaTests
{
    [Fact]
    public void WindowInsideWorkArea_NoChange()
    {
        var window = new MainWindow.RECT { Left = 100, Top = 100, Right = 440, Bottom = 500 };
        var workArea = new MainWindow.RECT { Left = 0, Top = 0, Right = 1920, Bottom = 1040 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(100, result.Left);
        Assert.Equal(100, result.Top);
    }

    [Fact]
    public void WindowOnSecondaryMonitor_StaysOnSecondary()
    {
        // Window at x=2100 on a secondary monitor spanning 1920..3840
        var window = new MainWindow.RECT { Left = 2100, Top = 300, Right = 2440, Bottom = 700 };
        var workArea = new MainWindow.RECT { Left = 1920, Top = 0, Right = 3840, Bottom = 1080 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(2100, result.Left);
        Assert.Equal(300, result.Top);
    }

    [Fact]
    public void WindowExceedingRightEdge_ClampsToRight()
    {
        var window = new MainWindow.RECT { Left = 1700, Top = 100, Right = 2040, Bottom = 500 };
        var workArea = new MainWindow.RECT { Left = 0, Top = 0, Right = 1920, Bottom = 1040 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(1580, result.Left); // 1920 - 340 width
        Assert.Equal(100, result.Top);
    }

    [Fact]
    public void WindowExceedingBottomEdge_ClampsToBottom()
    {
        var window = new MainWindow.RECT { Left = 100, Top = 900, Right = 440, Bottom = 1300 };
        var workArea = new MainWindow.RECT { Left = 0, Top = 0, Right = 1920, Bottom = 1040 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(100, result.Left);
        Assert.Equal(640, result.Top); // 1040 - 400 height
    }

    [Fact]
    public void WindowOnLeftSecondaryMonitor_NegativeCoordinatesPreserved()
    {
        // Secondary monitor to the left with negative X
        var window = new MainWindow.RECT { Left = -1500, Top = 200, Right = -1160, Bottom = 600 };
        var workArea = new MainWindow.RECT { Left = -1920, Top = 0, Right = 0, Bottom = 1080 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(-1500, result.Left);
        Assert.Equal(200, result.Top);
    }

    [Fact]
    public void WindowAbovePrimaryMonitor_NegativeYPreserved()
    {
        // Secondary monitor above primary with negative Y
        var window = new MainWindow.RECT { Left = 100, Top = -800, Right = 440, Bottom = -400 };
        var workArea = new MainWindow.RECT { Left = 0, Top = -1080, Right = 1920, Bottom = 0 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(100, result.Left);
        Assert.Equal(-800, result.Top);
    }

    [Fact]
    public void WindowBeyondLeftEdge_ClampsToLeft()
    {
        var window = new MainWindow.RECT { Left = -50, Top = 100, Right = 290, Bottom = 500 };
        var workArea = new MainWindow.RECT { Left = 0, Top = 0, Right = 1920, Bottom = 1040 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(0, result.Left);
        Assert.Equal(100, result.Top);
    }

    [Fact]
    public void WindowLargerThanWorkArea_AlignsToTopLeft()
    {
        var window = new MainWindow.RECT { Left = 100, Top = 100, Right = 2100, Bottom = 1200 };
        var workArea = new MainWindow.RECT { Left = 0, Top = 0, Right = 1920, Bottom = 1040 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(0, result.Left);
        Assert.Equal(0, result.Top);
    }

    [Fact]
    public void PreservesWindowSize()
    {
        var window = new MainWindow.RECT { Left = 2100, Top = 300, Right = 2440, Bottom = 700 };
        var workArea = new MainWindow.RECT { Left = 1920, Top = 0, Right = 3840, Bottom = 1080 };

        var result = MainWindow.ClampToWorkArea(window, workArea);

        Assert.Equal(340, result.Right - result.Left);
        Assert.Equal(400, result.Bottom - result.Top);
    }

    [Fact]
    public void CalculateMaxHeightFromWorkArea_ConvertsPhysicalPixelsToDipsAndSubtractsPadding()
    {
        var transformFromDevice = new Matrix(0.5, 0, 0, 0.5, 0, 0);

        var result = MainWindow.CalculateMaxHeightFromWorkArea(
            1800,
            transformFromDevice,
            minHeight: 180,
            padding: MainWindow.WorkAreaEdgePadding);

        Assert.Equal(896, result);
    }

    [Fact]
    public void CalculateMaxHeightFromWorkArea_WhenWorkAreaTooSmall_ReturnsMinHeight()
    {
        var result = MainWindow.CalculateMaxHeightFromWorkArea(100, Matrix.Identity, minHeight: 180, padding: 16);

        Assert.Equal(180, result);
    }

    [Fact]
    public void ShouldEnsureOnScreenAfterSizeChanged_WhenResizeInProgress_ReturnsFalse()
    {
        var result = MainWindow.ShouldEnsureOnScreenAfterSizeChanged(isResizeInProgress: true);

        Assert.False(result);
    }

    [Fact]
    public void ShouldEnsureOnScreenAfterSizeChanged_WhenResizeNotInProgress_ReturnsTrue()
    {
        var result = MainWindow.ShouldEnsureOnScreenAfterSizeChanged(isResizeInProgress: false);

        Assert.True(result);
    }
}

[Collection("WPF UI")]
public sealed class MainWindowPositionTests : IDisposable
{
    private readonly string tempDir;
    private readonly WpfApplicationFixture wpfApplicationFixture;

    public MainWindowPositionTests(WpfApplicationFixture wpfApplicationFixture)
    {
        this.wpfApplicationFixture = wpfApplicationFixture;
        this.tempDir = Path.Combine(Path.GetTempPath(), $"codexbar-app-test-{Guid.NewGuid():N}");
        Directory.CreateDirectory(this.tempDir);
    }

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(this.tempDir))
            {
                Directory.Delete(this.tempDir, recursive: true);
            }
        }
        catch
        {
            // Best effort cleanup
        }
    }

    [Fact]
    public void SaveWindowState_PersistsCurrentCoordinates()
    {
        this.wpfApplicationFixture.Run(() =>
        {
            var settingsService = this.CreateSettingsService();
            var settings = settingsService.Load();
            settings.WindowWidth = 340;
            settings.WindowHeight = 420;
            settingsService.Save(settings);

            var window = new MainWindow(settingsService)
            {
                Left = 123,
                Top = 456,
                Width = 340,
                Height = 420,
            };

            window.SaveWindowState();

            var saved = settingsService.Load();
            Assert.Equal(123, saved.WindowLeft);
            Assert.Equal(456, saved.WindowTop);
        });
    }

    [Fact]
    public void RestoreState_ReappliesSavedCoordinates()
    {
        this.wpfApplicationFixture.Run(() =>
        {
            var settingsService = this.CreateSettingsService();
            var settings = settingsService.Load();
            settings.WindowLeft = 321;
            settings.WindowTop = 654;
            settings.WindowWidth = 340;
            settings.WindowHeight = 420;
            settingsService.Save(settings);

            var window = new MainWindow(settingsService)
            {
                Left = 10,
                Top = 20,
            };

            window.Left = 42;
            window.Top = 24;

            window.RestoreState();

            Assert.Equal(321, window.Left);
            Assert.Equal(654, window.Top);
        });
    }

    [Fact]
    public void SaveWindowState_PersistsSavedCoordinates_WhenPositionRestoredFromSettings()
    {
        // Regression test: After Hide(), WPF may reset Left/Top.
        // SaveWindowState should use the in-memory savedLeft/savedTop values
        // (which were set from settings at startup) instead of WPF Left/Top.
        this.wpfApplicationFixture.Run(() =>
        {
            var settingsService = this.CreateSettingsService();
            var settings = settingsService.Load();
            settings.WindowLeft = 500;
            settings.WindowTop = 300;
            settings.WindowWidth = 340;
            settings.WindowHeight = 420;
            settingsService.Save(settings);

            var window = new MainWindow(settingsService);

            // Constructor loads savedLeft=500, savedTop=300 from settings,
            // and sets hasUserPosition=true.
            window.SaveWindowState();

            var saved = settingsService.Load();
            Assert.Equal(500, saved.WindowLeft);
            Assert.Equal(300, saved.WindowTop);
        });
    }

    [Fact]
    public void Constructor_WhenWindowHeightSaved_RestoresManualHeight()
    {
        this.wpfApplicationFixture.Run(() =>
        {
            var settingsService = this.CreateSettingsService();
            var settings = settingsService.Load();
            settings.WindowHeight = 420;
            settingsService.Save(settings);

            var window = new MainWindow(settingsService);

            Assert.Equal(SizeToContent.Manual, window.SizeToContent);
            Assert.Equal(420, window.Height);
        });
    }

    [Fact]
    public void SaveWindowState_WhenHeightIsAutoSized_DoesNotPersistHeight()
    {
        this.wpfApplicationFixture.Run(() =>
        {
            var settingsService = this.CreateSettingsService();
            var window = new MainWindow(settingsService)
            {
                Left = 123,
                Top = 456,
                Width = 340,
                Height = 420,
            };

            window.SaveWindowState();

            var saved = settingsService.Load();
            Assert.Null(saved.WindowHeight);
        });
    }

    private SettingsService CreateSettingsService() =>
        new(NullLogger<SettingsService>.Instance, this.tempDir);
}

[CollectionDefinition("WPF UI", DisableParallelization = true)]
public sealed class WpfUiCollectionDefinition : ICollectionFixture<WpfApplicationFixture>;

public sealed class WpfApplicationFixture : IDisposable
{
    // A guard against hangs, not a performance budget: the first Show() of a XAML window took over 10 s on a cold
    // GitHub Windows runner and failed main's CI (run 39348106450), while a passing test still returns in milliseconds.
    private static readonly TimeSpan HangTimeout = TimeSpan.FromSeconds(60);

    private readonly ManualResetEventSlim ready = new(false);
    private readonly Thread thread;
    private Application? application;
    private Dispatcher? dispatcher;
    private Exception? startupException;

    public WpfApplicationFixture()
    {
        EnsureWindowsDirectoryEnvironmentVariable();

        this.thread = new Thread(this.InitializeApplicationThread)
        {
            IsBackground = true,
        };

        this.thread.SetApartmentState(ApartmentState.STA);
        this.thread.Start();

        Assert.True(this.ready.Wait(HangTimeout), "WPF test application timed out during startup.");

        if (this.startupException is not null)
        {
            ExceptionDispatchInfo.Capture(this.startupException).Throw();
        }
    }

    public void Dispose()
    {
        if (this.dispatcher is null)
        {
            this.ready.Dispose();
            return;
        }

        this.dispatcher.Invoke(() => this.application?.Shutdown());
        this.dispatcher.InvokeShutdown();
        this.thread.Join();
        this.ready.Dispose();
    }

    public void Run(Action action)
    {
        if (this.dispatcher is null)
        {
            throw new InvalidOperationException("WPF test application was not initialized.");
        }

        var operation = this.dispatcher.InvokeAsync(action);
        Assert.True(operation.Task.Wait(HangTimeout), "STA test timed out.");
        operation.Task.GetAwaiter().GetResult();
    }

    private static void EnsureWindowsDirectoryEnvironmentVariable()
    {
        if (!string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("windir")))
        {
            return;
        }

        var systemRoot = Environment.GetEnvironmentVariable("SystemRoot");
        if (!string.IsNullOrWhiteSpace(systemRoot))
        {
            Environment.SetEnvironmentVariable("windir", systemRoot);
        }
    }

    private void InitializeApplicationThread()
    {
        try
        {
            this.application = new Application
            {
                ShutdownMode = ShutdownMode.OnExplicitShutdown,
            };

            RegisterApplicationResources(this.application.Resources);
            this.dispatcher = Dispatcher.CurrentDispatcher;
        }
        catch (Exception ex)
        {
            this.startupException = ex;
        }
        finally
        {
            this.ready.Set();
        }

        if (this.startupException is null)
        {
            Dispatcher.Run();
        }
    }

    private static void RegisterApplicationResources(ResourceDictionary resources)
    {
        resources["OpenRouterColor"] = new SolidColorBrush(Color.FromRgb(0x63, 0x66, 0xF1));
        resources["CopilotColor"] = new SolidColorBrush(Color.FromRgb(0x23, 0x86, 0x36));
        resources["BackgroundBrush"] = new SolidColorBrush(Color.FromRgb(0x1E, 0x1E, 0x2E));
        resources["SurfaceBrush"] = new SolidColorBrush(Color.FromRgb(0x2A, 0x2A, 0x3E));
        resources["TextPrimaryBrush"] = new SolidColorBrush(Color.FromRgb(0xE0, 0xE0, 0xE0));
        resources["TextSecondaryBrush"] = new SolidColorBrush(Color.FromRgb(0x9C, 0xA3, 0xAF));
    }
}
