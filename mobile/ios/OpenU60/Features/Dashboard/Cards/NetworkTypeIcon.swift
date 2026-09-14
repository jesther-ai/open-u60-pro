import SwiftUI

struct NetworkTypeIcon: View {
    let networkType: String

    // Fixed point sizes ignore Dynamic Type; scaling them (and the pill they sit in) keeps the
    // default-size rendering identical while letting the whole pill grow with the text size.
    @ScaledMetric(relativeTo: .subheadline) private var labelSize: CGFloat = 14
    @ScaledMetric(relativeTo: .caption2) private var subtitleSize: CGFloat = 9
    @ScaledMetric(relativeTo: .subheadline) private var pillWidth: CGFloat = 44
    @ScaledMetric(relativeTo: .subheadline) private var pillHeight: CGFloat = 28
    @ScaledMetric(relativeTo: .subheadline) private var twoLinePillHeight: CGFloat = 32

    var body: some View {
        if isNoService {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: labelSize, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: pillWidth, height: pillHeight)
                .background(pillColor, in: Capsule())
                .accessibilityLabel("No service")
        } else if isTwoLine {
            VStack(spacing: 0) {
                Text(generation)
                    .font(.system(size: labelSize, weight: .bold, design: .rounded))
                Text(subtitle)
                    .font(.system(size: subtitleSize, weight: .semibold, design: .rounded))
            }
            .foregroundStyle(.white)
            .frame(width: pillWidth, height: twoLinePillHeight)
            .background(pillColor, in: Capsule())
            .accessibilityElement(children: .combine)
        } else {
            Text(label)
                .font(.system(size: labelSize, weight: .bold, design: .rounded))
                .foregroundStyle(.white)
                .frame(width: pillWidth, height: pillHeight)
                .background(pillColor, in: Capsule())
        }
    }

    private var isNoService: Bool {
        let raw = networkType.lowercased()
        return raw.contains("limited") || raw.contains("no service")
    }

    private var isTwoLine: Bool {
        networkType == "5G SA" || networkType == "5G NSA"
    }

    private var generation: String {
        switch networkType {
        case "5G SA", "5G NSA": return "5G"
        default: return networkType
        }
    }

    private var subtitle: String {
        switch networkType {
        case "5G SA": return "SA"
        case "5G NSA": return "NSA"
        default: return ""
        }
    }

    private var label: String {
        networkType.isEmpty ? "--" : networkType
    }

    private var pillColor: Color {
        switch networkType {
        case "5G SA": return .blue
        case "5G NSA": return .teal
        case "4G+", "4G": return .green
        default:
            if isNoService { return .orange }
            if networkType.contains("3G") || networkType.contains("WCDMA")
                || networkType.contains("UMTS") || networkType.contains("GSM")
                || networkType.contains("2G") {
                return .orange
            }
            return .gray
        }
    }
}
