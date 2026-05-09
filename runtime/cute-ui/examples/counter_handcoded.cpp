// Hand-written equivalent of what `cute build` produces from a Counter
// .cute source. Useful for diagnosing the cute_ui side without going
// through the cute compiler.

#include <cute/ui/app.hpp>
#include <cute/ui/component.hpp>
#include <cute/ui/element.hpp>
#include <cute/ui/widgets.hpp>
#include <cute/ui/window.hpp>

#include <QObject>
#include <QString>

class Counter : public QObject
{
    Q_OBJECT
    Q_PROPERTY(int count READ count WRITE setCount NOTIFY countChanged)
public:
    explicit Counter(QObject *parent = nullptr) : QObject(parent) {}

    int count() const { return count_; }
    void setCount(int v) {
        if (count_ != v) {
            count_ = v;
            emit countChanged();
        }
    }

public slots:
    void increment() { setCount(count_ + 1); }
    void decrement() { setCount(count_ - 1); }

signals:
    void countChanged();

private:
    int count_ = 0;
};

class MainView : public cute::ui::Component
{
    Q_OBJECT
public:
    explicit MainView(QObject *parent = nullptr)
        : cute::ui::Component(parent), counter_(new Counter(this))
    {
        connect(counter_, &Counter::countChanged, this,
                [this] { requestRebuild(); });
    }

    std::unique_ptr<cute::ui::Element> build() override {
        using namespace cute::ui::dsl;

        auto countText = text(QStringLiteral("Count: %1").arg(counter_->count()));

        auto plusBtn  = button(QStringLiteral("+1"));
        plusBtn->onClick([this] { counter_->increment(); });

        auto minusBtn = button(QStringLiteral("-1"));
        minusBtn->onClick([this] { counter_->decrement(); });

        return col(
            std::move(countText),
            row(std::move(plusBtn), std::move(minusBtn))
        );
    }

private:
    Counter *counter_;
};

#include "counter_handcoded.moc"

int main(int argc, char **argv)
{
    cute::ui::App app(argc, argv);

    MainView view;
    return app.run(&view);
}
