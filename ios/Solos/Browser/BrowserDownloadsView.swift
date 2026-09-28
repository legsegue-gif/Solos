//  BrowserDownloadsView.swift — what the browser has pulled down.
//
//  The files are already in the workspace, so this is not a file manager:
//  it says what is in flight, what landed and where, and lets you stop one.
//  Reading them is the files page's job, and the model's.

import SwiftUI

struct BrowserDownloadsView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var center = DownloadCenter.shared

    var body: some View {
        Group {
            if center.items.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "arrow.down.circle")
                        .font(.largeTitle)
                        .foregroundStyle(.tertiary)
                    Text("No downloads yet").font(.headline)
                    Text("Files a page downloads land in /solos/ws/downloads, where the model can read them too.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .padding(32)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List {
                    ForEach(center.items.reversed()) { item in
                        row(item)
                    }
                }
                .listStyle(.plain)
            }
        }
        .navigationTitle(String(localized: "Downloads"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Done") { dismiss() }
            }
        }
    }

    private func row(_ item: DownloadCenter.Item) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(item.name).font(.callout).lineLimit(1)
                Spacer()
                if item.state == .running {
                    Button("Cancel") { center.cancel(item.id) }
                        .font(.caption)
                        .buttonStyle(.borderless)
                }
            }
            Text(detail(item))
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .padding(.vertical, 2)
    }

    private func detail(_ item: DownloadCenter.Item) -> String {
        switch item.state {
        case .running: return String(localized: "Downloading…")
        case .finished: return item.solosURL ?? String(localized: "Finished")
        case .failed: return String(localized: "Failed: \(item.error ?? String(localized: "no reason given"))")
        case .cancelled: return String(localized: "Cancelled")
        }
    }
}
