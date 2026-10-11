#include "texthelper.h"

#include <utility>

#include <QtCore/QJsonArray>
#include <QtCore/QJsonDocument>
#include <QtCore/QJsonObject>
#include <QtCore/QTimer>
#include <QtCore/QVector>
#include <QtGui/QClipboard>
#include <QtGui/QGuiApplication>
#include <QtGui/QSyntaxHighlighter>
#include <QtGui/QTextBlock>
#include <QtGui/QTextCursor>
#include <QtGui/QTextDocument>

namespace {

QString normalized(QString text)
{
    text.replace(QChar::ParagraphSeparator, QLatin1Char('\n'));
    text.replace(QChar::LineSeparator, QLatin1Char('\n'));
    return text;
}

int documentLength(const QTextDocument *doc)
{
    // characterCount() includes the final block's implicit separator.
    return doc ? doc->characterCount() - 1 : 0;
}

} // namespace

class RegionHighlighter : public QSyntaxHighlighter
{
public:
    struct Range {
        int start;
        int end;
        int kind; // 0 revisable, 1 provisional
    };

    explicit RegionHighlighter(QTextDocument *doc)
        : QSyntaxHighlighter(doc)
    {
    }

    QVector<Range> ranges;
    QTextCharFormat formats[2];

    void rehighlightRange(int start, int end)
    {
        QTextDocument *doc = document();
        if (!doc)
            return;
        QTextBlock block = doc->findBlock(qMax(0, start));
        const QTextBlock last = doc->findBlock(qMax(0, end));
        while (block.isValid()) {
            rehighlightBlock(block);
            if (block == last)
                break;
            block = block.next();
        }
    }

protected:
    void highlightBlock(const QString &text) override
    {
        const int blockStart = currentBlock().position();
        const int blockEnd = blockStart + int(text.length());
        for (const Range &r : std::as_const(ranges)) {
            const int s = qMax(r.start, blockStart);
            const int e = qMin(r.end, blockEnd);
            if (e > s)
                setFormat(s - blockStart, e - s, formats[r.kind]);
        }
    }
};

TextHelper::TextHelper(QObject *parent)
    : QObject(parent)
    , m_revisableColor(QColor(80, 140, 255, 40))
    , m_provisionalColor(QColor(80, 140, 255, 90))
    , m_provisionalTextColor(QColor(90, 90, 90))
{
}

TextHelper::~TextHelper() = default;

QQuickTextDocument *TextHelper::document() const
{
    return m_document;
}

void TextHelper::setDocument(QQuickTextDocument *document)
{
    if (m_document == document)
        return;
    if (m_connection)
        QObject::disconnect(m_connection);
    delete m_highlighter;
    m_highlighter = nullptr;
    m_document = document;
    m_textDocument = document ? document->textDocument() : nullptr;
    if (m_textDocument) {
        m_textDocument->setUndoRedoEnabled(false);
        // Connect before creating the highlighter so this handler sees each change before the
        // highlighter's own (format-only) change notifications.
        m_connection = connect(m_textDocument, &QTextDocument::contentsChange, this,
                               &TextHelper::onContentsChange);
        m_highlighter = new RegionHighlighter(m_textDocument);
        updateFormats();
    }
    Q_EMIT documentChanged();
}

bool TextHelper::applying() const
{
    return m_applying;
}

void TextHelper::setApplying(bool applying)
{
    if (m_applying == applying)
        return;
    m_applying = applying;
    Q_EMIT applyingChanged();
}

QColor TextHelper::revisableColor() const
{
    return m_revisableColor;
}

void TextHelper::setRevisableColor(const QColor &color)
{
    m_revisableColor = color;
    updateFormats();
    Q_EMIT colorsChanged();
}

QColor TextHelper::provisionalColor() const
{
    return m_provisionalColor;
}

void TextHelper::setProvisionalColor(const QColor &color)
{
    m_provisionalColor = color;
    updateFormats();
    Q_EMIT colorsChanged();
}

QColor TextHelper::provisionalTextColor() const
{
    return m_provisionalTextColor;
}

void TextHelper::setProvisionalTextColor(const QColor &color)
{
    m_provisionalTextColor = color;
    updateFormats();
    Q_EMIT colorsChanged();
}

void TextHelper::updateFormats()
{
    if (!m_highlighter)
        return;
    QTextCharFormat revisable;
    revisable.setBackground(m_revisableColor);
    QTextCharFormat provisional;
    provisional.setBackground(m_provisionalColor);
    provisional.setForeground(m_provisionalTextColor);
    provisional.setFontItalic(true);
    m_highlighter->formats[0] = revisable;
    m_highlighter->formats[1] = provisional;
    const bool wasApplying = m_applying;
    setApplying(true);
    m_highlighter->rehighlight();
    setApplying(wasApplying);
}

void TextHelper::onContentsChange(int position, int removed, int added)
{
    if (m_applying || !m_textDocument)
        return;
    const int length = documentLength(m_textDocument);
    const int end = qMin(position + added, length);
    QTextCursor cursor(m_textDocument);
    cursor.setPosition(qBound(0, position, length));
    cursor.setPosition(qBound(0, end, length), QTextCursor::KeepAnchor);
    const QString inserted = normalized(cursor.selectedText());
    ++m_inChange;
    Q_EMIT userEdited(position, removed, inserted, length);
    --m_inChange;
}

void TextHelper::setText(const QString &text)
{
    if (!m_textDocument)
        return;
    setApplying(true);
    QTextCursor cursor(m_textDocument);
    cursor.select(QTextCursor::Document);
    cursor.insertText(text);
    setApplying(false);
}

void TextHelper::applyEdits(const QString &json)
{
    const QJsonArray edits = QJsonDocument::fromJson(json.toUtf8()).array();
    if (edits.isEmpty())
        return;
    // Keep order: once anything is deferred, later edits queue behind it.
    if (m_inChange > 0 || !m_queue.isEmpty()) {
        m_queue.append(edits);
        scheduleFlush();
        return;
    }
    applyNow(edits);
}

void TextHelper::scheduleFlush()
{
    if (m_flushScheduled)
        return;
    m_flushScheduled = true;
    QTimer::singleShot(0, this, [this] {
        m_flushScheduled = false;
        const QList<QJsonArray> queue = std::exchange(m_queue, {});
        for (const QJsonArray &edits : queue)
            applyNow(edits);
        if (!m_pendingHighlights.isNull())
            setHighlights(std::exchange(m_pendingHighlights, QString()));
    });
}

void TextHelper::applyNow(const QJsonArray &edits)
{
    if (!m_textDocument)
        return;
    setApplying(true);
    for (const QJsonValue &value : edits) {
        const QJsonObject edit = value.toObject();
        const int length = documentLength(m_textDocument);
        const int position = qBound(0, edit.value(QLatin1String("position")).toInt(), length);
        const int remove = qBound(0, edit.value(QLatin1String("remove")).toInt(), length - position);
        QTextCursor cursor(m_textDocument);
        cursor.setPosition(position);
        cursor.setPosition(position + remove, QTextCursor::KeepAnchor);
        cursor.insertText(edit.value(QLatin1String("insert")).toString());
    }
    setApplying(false);
}

void TextHelper::setHighlights(const QString &json)
{
    if (!m_highlighter)
        return;
    if (m_inChange > 0 || !m_queue.isEmpty()) {
        m_pendingHighlights = json;
        scheduleFlush();
        return;
    }
    QVector<RegionHighlighter::Range> ranges;
    const QJsonArray items = QJsonDocument::fromJson(json.toUtf8()).array();
    for (const QJsonValue &value : items) {
        const QJsonArray item = value.toArray();
        if (item.size() < 3)
            continue;
        const int kind = item.at(2).toString() == QLatin1String("provisional") ? 1 : 0;
        ranges.append({item.at(0).toInt(), item.at(1).toInt(), kind});
    }
    int lo = INT_MAX;
    int hi = -1;
    for (const auto *list : {&m_highlighter->ranges, &ranges}) {
        for (const auto &r : *list) {
            lo = qMin(lo, r.start);
            hi = qMax(hi, r.end);
        }
    }
    m_highlighter->ranges = ranges;
    if (hi < 0)
        return;
    setApplying(true);
    m_highlighter->rehighlightRange(lo, qMin(hi, documentLength(m_textDocument)));
    setApplying(false);
}

QString TextHelper::plainText() const
{
    return m_textDocument ? normalized(m_textDocument->toRawText()) : QString();
}

void TextHelper::copyToClipboard(const QString &text) const
{
    QGuiApplication::clipboard()->setText(text);
}

int TextHelper::length() const
{
    return documentLength(m_textDocument);
}
