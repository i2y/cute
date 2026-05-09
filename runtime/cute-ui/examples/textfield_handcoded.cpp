// Hand-written TextField smoke test for the cute_ui runtime. Matches the
// shape that codegen will lower a `widget` containing TextField + Text into.

#include <cute/ui/app.hpp>
#include <cute/ui/component.hpp>
#include <cute/ui/element.hpp>
#include <cute/ui/widgets.hpp>
#include <cute/ui/window.hpp>

#include <QObject>
#include <QString>

class NoteStore : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QString text READ text WRITE setText NOTIFY textChanged)
public:
    explicit NoteStore(QObject *parent = nullptr) : QObject(parent) {}
    QString text() const { return text_; }
    void setText(const QString &v) {
        if (text_ != v) { text_ = v; emit textChanged(); }
    }
signals:
    void textChanged();
private:
    QString text_;
};

class MainView : public cute::ui::Component
{
    Q_OBJECT
public:
    explicit MainView(QObject *parent = nullptr)
        : cute::ui::Component(parent), store_(new NoteStore(this))
    {
        connect(store_, &NoteStore::textChanged, this,
                [this] { requestRebuild(); });
    }

    std::unique_ptr<cute::ui::Element> build() override {
        using namespace cute::ui::dsl;

        auto label = text(QStringLiteral("Length: %1").arg(store_->text().size()));

        auto field = textfield(QStringLiteral("Type something..."));
        field->setText(store_->text());
        field->setOnTextChanged([this](QString s) { store_->setText(s); });

        auto preview = text(store_->text().isEmpty()
            ? QStringLiteral("(nothing yet)")
            : QStringLiteral("Echo: ") + store_->text());

        return col(
            std::move(label),
            std::move(field),
            std::move(preview)
        );
    }

private:
    NoteStore *store_;
};

#include "textfield_handcoded.moc"

int main(int argc, char **argv)
{
    cute::ui::App app(argc, argv);
    MainView view;
    return app.run(&view);
}
