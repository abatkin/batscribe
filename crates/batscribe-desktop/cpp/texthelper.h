#pragma once

// Bridges a QML TextArea's QTextDocument to the Rust editor model.
//
// - Reports user changes (`userEdited`) with exact UTF-16 positions from
//   QTextDocument::contentsChange. Changes it makes itself are never reported.
// - Applies programmatic range edits (voice updates, reverts, undo) without echoing them.
//   Edits requested while a change notification is being handled are deferred, since the
//   document must not be modified from inside contentsChange.
// - Shades the voice region with a display-only QSyntaxHighlighter.
// - Turns off QTextDocument's own undo stack; undo is implemented in Rust.

#include <QtCore/QJsonArray>
#include <QtCore/QList>
#include <QtCore/QObject>
#include <QtCore/QPointer>
#include <QtGui/QColor>
#include <QtQml/qqmlregistration.h>
#include <QtQuick/QQuickTextDocument>

class QTextDocument;
class RegionHighlighter;

class TextHelper : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QQuickTextDocument *document READ document WRITE setDocument NOTIFY documentChanged)
    Q_PROPERTY(bool applying READ applying NOTIFY applyingChanged)
    Q_PROPERTY(QColor revisableColor READ revisableColor WRITE setRevisableColor NOTIFY colorsChanged)
    Q_PROPERTY(QColor provisionalColor READ provisionalColor WRITE setProvisionalColor NOTIFY colorsChanged)
    Q_PROPERTY(QColor provisionalTextColor READ provisionalTextColor WRITE setProvisionalTextColor NOTIFY colorsChanged)

public:
    explicit TextHelper(QObject *parent = nullptr);
    ~TextHelper() override;

    QQuickTextDocument *document() const;
    void setDocument(QQuickTextDocument *document);
    bool applying() const;

    QColor revisableColor() const;
    void setRevisableColor(const QColor &color);
    QColor provisionalColor() const;
    void setProvisionalColor(const QColor &color);
    QColor provisionalTextColor() const;
    void setProvisionalTextColor(const QColor &color);

    // Replaces the whole text (document load).
    Q_INVOKABLE void setText(const QString &text);
    // JSON array of {"position", "remove", "insert"} in UTF-16 positions, applied in order.
    Q_INVOKABLE void applyEdits(const QString &json);
    // JSON array of [start, end, "revisable" | "provisional"].
    Q_INVOKABLE void setHighlights(const QString &json);
    // The document text with "\n" line endings.
    Q_INVOKABLE QString plainText() const;
    Q_INVOKABLE int length() const;
    // Puts `text` on the system clipboard (QML has no clipboard API).
    Q_INVOKABLE void copyToClipboard(const QString &text) const;

Q_SIGNALS:
    void documentChanged();
    void applyingChanged();
    void colorsChanged();
    // `removed` UTF-16 units at `position` were replaced by `inserted`; `newLength` is the
    // document length afterwards.
    void userEdited(int position, int removed, const QString &inserted, int newLength);

private:
    void onContentsChange(int position, int removed, int added);
    void applyNow(const QJsonArray &edits);
    void scheduleFlush();
    void setApplying(bool applying);
    void updateFormats();

    QPointer<QQuickTextDocument> m_document;
    QPointer<QTextDocument> m_textDocument;
    RegionHighlighter *m_highlighter = nullptr;
    QMetaObject::Connection m_connection;
    bool m_applying = false;
    int m_inChange = 0;
    bool m_flushScheduled = false;
    QList<QJsonArray> m_queue;
    QString m_pendingHighlights;
    QColor m_revisableColor;
    QColor m_provisionalColor;
    QColor m_provisionalTextColor;
};
