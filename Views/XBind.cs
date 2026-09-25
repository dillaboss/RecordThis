using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using RecordThis.Models;

namespace RecordThis.Views;

/// <summary>Static helpers for x:Bind function bindings (compile-time, no converters or reflection).</summary>
public static class XBind
{
    public static bool IsNotNull(object? value) => value is not null;

    public static Visibility VisibleIfNotNull(object? value) => value is null ? Visibility.Collapsed : Visibility.Visible;

    public static bool Not(bool value) => !value;

    public static Visibility VisibleIf(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility CollapsedIf(bool value) => value ? Visibility.Collapsed : Visibility.Visible;

    public static double OpaqueIf(bool value) => value ? 1.0 : 0.0;

    public static InfoBarSeverity Severity(Notice? notice) => notice?.Severity switch
    {
        NoticeSeverity.Success => InfoBarSeverity.Success,
        NoticeSeverity.Warning => InfoBarSeverity.Warning,
        NoticeSeverity.Error => InfoBarSeverity.Error,
        _ => InfoBarSeverity.Informational,
    };

    public static Visibility HasAction(Notice? notice) =>
        notice is { Action: not NoticeAction.None, ActionText: not null } ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Record (E7C8) while idle, Stop (E71A) while recording.</summary>
    public static string RecordGlyph(bool isRecording) => isRecording ? "\uE71A" : "\uE7C8";

    public static string NoticeTitle(Notice? notice) => notice?.Title ?? string.Empty;

    public static string NoticeMessage(Notice? notice) => notice?.Message ?? string.Empty;

    public static string NoticeActionText(Notice? notice) => notice?.ActionText ?? string.Empty;
}
