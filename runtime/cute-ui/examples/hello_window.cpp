// Standalone smoke test: opens a QWindow, drives a QRhi swap chain, and
// renders rounded rects + SDF text via QCanvasPainterFactory directly.
// Useful for diagnosing Canvas Painter / QRhi issues without the cute::ui
// abstraction in the way.

#include <cute/ui/version.hpp>

#include <QGuiApplication>
#include <QWindow>
#include <QExposeEvent>
#include <QResizeEvent>
#include <QPlatformSurfaceEvent>

#include <rhi/qrhi.h>

#include <QtCanvasPainter/QCanvasPainter>
#include <QtCanvasPainter/QCanvasPainterFactory>
#include <QtCanvasPainter/QCanvasRhiPaintDriver>

#include <QColor>
#include <QFont>
#include <QDebug>
#include <memory>

namespace {

class HelloWindow : public QWindow
{
    Q_OBJECT
public:
    explicit HelloWindow(QRhi::Implementation api);
    ~HelloWindow() override;

protected:
    void exposeEvent(QExposeEvent *) override;
    bool event(QEvent *e) override;

private:
    void init();
    void resizeSwapChain();
    void releaseResources();
    void render();

    QRhi::Implementation graphicsApi_;
    std::unique_ptr<QRhi> rhi_;
    std::unique_ptr<QRhiSwapChain> sc_;
    std::unique_ptr<QRhiRenderBuffer> ds_;
    std::unique_ptr<QRhiRenderPassDescriptor> rp_;
    std::unique_ptr<QCanvasPainterFactory> canvasFactory_;
    bool hasSwapChain_ = false;
    bool notExposed_ = false;
    QSize lastPixelSize_;
};

HelloWindow::HelloWindow(QRhi::Implementation api)
    : graphicsApi_(api)
{
    switch (api) {
    case QRhi::Metal:        setSurfaceType(QSurface::MetalSurface); break;
    case QRhi::D3D11:
    case QRhi::D3D12:        setSurfaceType(QSurface::Direct3DSurface); break;
    case QRhi::Vulkan:       setSurfaceType(QSurface::VulkanSurface); break;
    case QRhi::OpenGLES2:    setSurfaceType(QSurface::OpenGLSurface); break;
    case QRhi::Null:         setSurfaceType(QSurface::RasterSurface); break;
    }
    setTitle(QStringLiteral("cute::ui hello"));
    resize(640, 480);
}

HelloWindow::~HelloWindow()
{
    releaseResources();
}

void HelloWindow::init()
{
    QRhi *rawRhi = nullptr;
#if QT_CONFIG(metal)
    if (graphicsApi_ == QRhi::Metal) {
        QRhiMetalInitParams params;
        rawRhi = QRhi::create(QRhi::Metal, &params);
    }
#endif
#if defined(Q_OS_WIN)
    if (graphicsApi_ == QRhi::D3D12) {
        QRhiD3D12InitParams params;
        rawRhi = QRhi::create(QRhi::D3D12, &params);
    }
#endif
    if (graphicsApi_ == QRhi::Null) {
        QRhiInitParams params;
        rawRhi = QRhi::create(QRhi::Null, &params);
    }
    rhi_.reset(rawRhi);
    if (!rhi_) qFatal("Failed to create QRhi for selected graphics API.");

    sc_.reset(rhi_->newSwapChain());
    ds_.reset(rhi_->newRenderBuffer(QRhiRenderBuffer::DepthStencil,
                                    QSize(), 1,
                                    QRhiRenderBuffer::UsedWithSwapChainOnly));
    sc_->setWindow(this);
    sc_->setDepthStencil(ds_.get());
    sc_->setSampleCount(1);
    rp_.reset(sc_->newCompatibleRenderPassDescriptor());
    sc_->setRenderPassDescriptor(rp_.get());

    // sharedInstance() alone leaves paintDriver()/painter() unusable.
    // Following the Compact Health example, the factory is created here
    // explicitly with the QRhi instance.
    canvasFactory_ = std::make_unique<QCanvasPainterFactory>();
    canvasFactory_->create(rhi_.get());
    if (!canvasFactory_->isValid()) {
        qFatal("QCanvasPainterFactory::create() failed.");
    }
}

void HelloWindow::releaseResources()
{
    // Order matters: the QRhi must outlive every resource referencing it.
    canvasFactory_.reset();
    rp_.reset();
    ds_.reset();
    sc_.reset();
    rhi_.reset();
    hasSwapChain_ = false;
}

void HelloWindow::resizeSwapChain()
{
    hasSwapChain_ = sc_->createOrResize();
}

void HelloWindow::exposeEvent(QExposeEvent *)
{
    if (isExposed() && !rhi_) {
        init();
        resizeSwapChain();
        lastPixelSize_ = sc_->surfacePixelSize();
    }

    const QSize surfaceSize = sc_ ? sc_->surfacePixelSize() : QSize();
    if ((!isExposed() || (hasSwapChain_ && surfaceSize.isEmpty())) && rhi_) {
        notExposed_ = true;
    }
    if (isExposed() && rhi_ && hasSwapChain_ && !surfaceSize.isEmpty()) {
        notExposed_ = false;
        if (surfaceSize != lastPixelSize_) {
            resizeSwapChain();
            lastPixelSize_ = surfaceSize;
        }
        render();
    }
}

bool HelloWindow::event(QEvent *e)
{
    switch (e->type()) {
    case QEvent::UpdateRequest:
        render();
        break;
    case QEvent::PlatformSurface:
        if (static_cast<QPlatformSurfaceEvent *>(e)->surfaceEventType()
            == QPlatformSurfaceEvent::SurfaceAboutToBeDestroyed) {
            releaseResources();
        }
        break;
    default:
        break;
    }
    return QWindow::event(e);
}

void HelloWindow::render()
{
    if (!hasSwapChain_ || notExposed_) return;

    const QSize currentPixelSize = sc_->surfacePixelSize();
    if (currentPixelSize != lastPixelSize_) {
        resizeSwapChain();
        lastPixelSize_ = currentPixelSize;
        if (!hasSwapChain_) return;
    }

    QRhi::FrameOpResult r = rhi_->beginFrame(sc_.get());
    if (r == QRhi::FrameOpSwapChainOutOfDate) {
        resizeSwapChain();
        if (!hasSwapChain_) return;
        r = rhi_->beginFrame(sc_.get());
    }
    if (r != QRhi::FrameOpSuccess) {
        requestUpdate();
        return;
    }

    QRhiCommandBuffer *cb = sc_->currentFrameCommandBuffer();
    QRhiRenderTarget *rt = sc_->currentFrameRenderTarget();

    QCanvasRhiPaintDriver *driver = canvasFactory_->paintDriver();
    QCanvasPainter *painter = canvasFactory_->painter();

    // resetForNewFrame must come before beginPaint each frame.
    driver->resetForNewFrame();

    const float dpr = static_cast<float>(devicePixelRatio());
    driver->beginPaint(cb, rt, QColor(20, 22, 30), size(), dpr);

    painter->setFillStyle(QColor(70, 130, 200));
    painter->fillRect(40.0f, 40.0f, 240.0f, 80.0f);

    painter->beginPath();
    painter->roundRect(40.0f, 140.0f, 240.0f, 80.0f, 16.0f);
    painter->setFillStyle(QColor(120, 200, 130));
    painter->fill();

    QFont font(QStringLiteral("Helvetica"), 18);
    painter->setFont(font);
    painter->setFillStyle(QColor(255, 255, 255));
    painter->fillText(QStringLiteral("Hello, cute::ui"), 40.0f, 280.0f);
    painter->fillText(QStringLiteral("こんにちは"), 40.0f, 310.0f);

    // Note: renderPaint() is offscreen-canvas only; for onscreen windows
    // QRhi::endFrame() submits and presents.
    driver->endPaint();
    rhi_->endFrame(sc_.get());

    requestUpdate();
}

} // namespace

#include "hello_window.moc"

int main(int argc, char **argv)
{
    QGuiApplication app(argc, argv);
    qInfo() << "cute_ui version:" << cute::ui::version();

#if defined(Q_OS_MACOS)
    HelloWindow window(QRhi::Metal);
#elif defined(Q_OS_WIN)
    HelloWindow window(QRhi::D3D12);
#else
    // Vulkan needs a QVulkanInstance set up beforehand; skipped here.
    HelloWindow window(QRhi::Null);
#endif
    window.show();

    return app.exec();
}
