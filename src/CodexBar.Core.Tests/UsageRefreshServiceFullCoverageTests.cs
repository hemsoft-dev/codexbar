// Copyright (c) HemSoft Developments. All rights reserved.

namespace CodexBar.Core.Tests;

using CodexBar.Core.Models;
using CodexBar.Core.Providers;
using CodexBar.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;
using NSubstitute;

/// <summary>
/// Full coverage tests for UsageRefreshService: NextRefreshChanged event,
/// Dispose/DisposeAsync paths, subscriber exceptions, and provider-unavailable
/// transition logic.
/// </summary>
public class UsageRefreshServiceFullCoverageTests
{
    // A guard against hangs, not a timing budget: two cycles of a 50 ms loop took over 2 s on a slow GitHub Windows
    // runner and failed CI (run 39356458890), while a passing wait still ends in milliseconds.
    private static readonly TimeSpan HangGuard = TimeSpan.FromSeconds(30);

    private static IUsageProvider CreateMockProvider(
        ProviderId id,
        bool available = true,
        ProviderUsageResult? result = null)
    {
        var provider = Substitute.For<IUsageProvider>();
        provider.Metadata.Returns(new ProviderMetadata { Id = id, DisplayName = id.ToString(), Description = "Test" });
        provider.IsAvailableAsync(Arg.Any<CancellationToken>()).Returns(Task.FromResult(available));
        provider.FetchUsageAsync(Arg.Any<CancellationToken>())
            .Returns(result ?? new ProviderUsageResult { Provider = id, Success = true });
        return provider;
    }

    [Fact]
    public async Task RefreshAllAsync_ProviderThrows_StoresFailure()
    {
        var provider = Substitute.For<IUsageProvider>();
        provider.Metadata.Returns(new ProviderMetadata { Id = ProviderId.OpenRouter, DisplayName = "OpenRouter", Description = "Test" });
        provider.IsAvailableAsync(Arg.Any<CancellationToken>()).Returns(Task.FromResult(true));
        provider.FetchUsageAsync(Arg.Any<CancellationToken>())
            .Returns<Task<ProviderUsageResult>>(_ => throw new InvalidOperationException("boom"));

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);

        await service.RefreshAllAsync();

        var results = service.LatestResults;
        Assert.True(results.ContainsKey(ProviderId.OpenRouter));
        Assert.False(results[ProviderId.OpenRouter].Success);
        Assert.Contains("boom", results[ProviderId.OpenRouter].ErrorMessage);
    }

    [Fact]
    public async Task RefreshAllAsync_ProviderUnavailable_RemovesPreviousResultAndNotifies()
    {
        var provider = Substitute.For<IUsageProvider>();
        provider.Metadata.Returns(new ProviderMetadata { Id = ProviderId.Claude, DisplayName = "Claude", Description = "Test" });
        provider.IsAvailableAsync(Arg.Any<CancellationToken>()).Returns(
            Task.FromResult(true),   // first call: available
            Task.FromResult(false)); // second call: unavailable
        provider.FetchUsageAsync(Arg.Any<CancellationToken>())
            .Returns(new ProviderUsageResult { Provider = ProviderId.Claude, Success = true });

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);

        var updatedEvents = new List<(ProviderId Id, ProviderUsageResult Result)>();
        service.UsageUpdated += (id, r) => updatedEvents.Add((id, r));

        // First refresh: available
        await service.RefreshAllAsync();
        Assert.True(service.LatestResults.ContainsKey(ProviderId.Claude));
        Assert.True(service.LatestResults[ProviderId.Claude].Success);

        // Second refresh: unavailable
        await service.RefreshAllAsync();
        Assert.True(service.LatestResults.ContainsKey(ProviderId.Claude));
        Assert.False(service.LatestResults[ProviderId.Claude].Success);

        // Should have fired UsageUpdated for the unavailable transition
        Assert.True(updatedEvents.Count >= 2);
    }

    [Fact]
    public async Task RefreshAllAsync_ProviderUnavailableFromStart_FiresFailureEvent()
    {
        var provider = CreateMockProvider(ProviderId.Claude, available: false);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);

        var updatedEvents = new List<ProviderId>();
        service.UsageUpdated += (id, _) => updatedEvents.Add(id);

        await service.RefreshAllAsync();

        Assert.Equal([ProviderId.Claude], updatedEvents);
        Assert.Single(service.LatestResults);
        Assert.False(service.LatestResults[ProviderId.Claude].Success);
    }

    [Fact]
    public async Task UsageUpdated_SubscriberThrows_DoesNotCrashService()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);

        service.UsageUpdated += (_, _) => throw new InvalidOperationException("subscriber error");

        // Should not throw
        await service.RefreshAllAsync();

        Assert.True(service.LatestResults.ContainsKey(ProviderId.OpenRouter));
    }

    [Fact]
    public async Task NextRefreshChanged_WhenSubscriberThrows_ServiceContinuesRunning()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromMilliseconds(50);

        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        service.NextRefreshChanged += _ =>
        {
            started.TrySetResult();
            throw new InvalidOperationException("subscriber error");
        };

        service.Start();
        await WaitUntilAsync(() => started.Task.IsCompleted, HangGuard);

        Assert.True(started.Task.IsCompleted);
        Assert.NotNull(service.NextRefreshAtUtc);

        await service.StopAsync();
    }

    [Fact]
    public async Task StartAndStop_WhenStarted_SetsAndClearsNextRefresh()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromMilliseconds(50);

        DateTimeOffset? lastChanged = null;
        service.NextRefreshChanged += val => lastChanged = val;

        service.Start();
        await WaitUntilAsync(() => service.NextRefreshAtUtc is not null, HangGuard);

        Assert.NotNull(service.NextRefreshAtUtc);

        await service.StopAsync();

        Assert.Null(service.NextRefreshAtUtc);
        Assert.Null(lastChanged); // Last event should be null (cleared)
    }

    [Fact]
    public void Start_CalledTwice_DoesNotCreateSecondLoop()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromHours(1);

        service.Start();
        service.Start(); // should no-op

        Assert.NotNull(service.NextRefreshAtUtc);
        service.Dispose();
        Assert.Null(service.NextRefreshAtUtc);
    }

    [Fact]
    public async Task StopAsync_WhenNotStarted_LeavesNextRefreshNull()
    {
        var service = new UsageRefreshService(
            [],
            NullLogger<UsageRefreshService>.Instance);

        await service.StopAsync();

        Assert.Null(service.NextRefreshAtUtc);
        Assert.Empty(service.LatestResults);
    }

    [Fact]
    public void Dispose_WhenStarted_StopsLoop()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromHours(1);

        service.Start();
        service.Dispose();

        Assert.Null(service.NextRefreshAtUtc);
    }

    [Fact]
    public void Dispose_WhenNotStarted_LeavesNextRefreshNull()
    {
        var service = new UsageRefreshService(
            [],
            NullLogger<UsageRefreshService>.Instance);

        service.Dispose();

        Assert.Null(service.NextRefreshAtUtc);
        Assert.Empty(service.LatestResults);
    }

    [Fact]
    public async Task DisposeAsync_WhenStarted_StopsService()
    {
        var provider = CreateMockProvider(ProviderId.OpenRouter);

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromHours(1);

        service.Start();
        await service.DisposeAsync();

        Assert.Null(service.NextRefreshAtUtc);
    }

    [Fact]
    public async Task RefreshLoop_WhenStarted_ExecutesMultipleCycles()
    {
        var callCount = 0;
        var provider = Substitute.For<IUsageProvider>();
        provider.Metadata.Returns(new ProviderMetadata { Id = ProviderId.OpenRouter, DisplayName = "OpenRouter", Description = "Test" });
        provider.IsAvailableAsync(Arg.Any<CancellationToken>()).Returns(Task.FromResult(true));
        provider.FetchUsageAsync(Arg.Any<CancellationToken>()).Returns(_ =>
        {
            Interlocked.Increment(ref callCount);
            return Task.FromResult(new ProviderUsageResult { Provider = ProviderId.OpenRouter, Success = true });
        });

        var service = new UsageRefreshService(
            [provider],
            NullLogger<UsageRefreshService>.Instance);
        service.RefreshInterval = TimeSpan.FromMilliseconds(50);

        service.Start();
        await WaitUntilAsync(() => callCount >= 2, HangGuard);
        await service.StopAsync();

        // Should have at least 2 fetches (initial + at least 1 loop)
        Assert.True(callCount >= 2, $"Expected >= 2 calls, got {callCount}");
    }

    private static async Task WaitUntilAsync(Func<bool> condition, TimeSpan timeout, TimeSpan? poll = null)
    {
        var interval = poll ?? TimeSpan.FromMilliseconds(10);
        var start = DateTime.UtcNow;
        while (!condition())
        {
            if (DateTime.UtcNow - start > timeout)
            {
                throw new TimeoutException("Condition was not met in time.");
            }

            await Task.Delay(interval);
        }
    }
}
