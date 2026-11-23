export const metadata = { title: 'Terms of Service · Milo', alternates: { canonical: 'https://usemilo.net/terms/' } };

export default function Terms() {
  return <article><h1>Terms of Service</h1>
    <p>These terms cover Milo’s optional alert integrations. You don’t need to connect an account or use an alert service to use the local Milo CLI.</p>
    <h2>Your destinations</h2><p>Only connect chats and channels you’re authorized to use. You choose which events to forward and whether they include program text or a Rex session label. Check the audience before enabling forwarding; don’t send secrets or personal data you aren’t authorized to share.</p>
    <h2>Acceptable use</h2><p>Don’t use Milo to send spam, harass people, impersonate others, evade service limits, or access someone else’s destination. Follow the terms of the platform you connect: <a href="https://discord.com/terms">Discord</a> or <a href="https://telegram.org/tos">Telegram</a>. Access may be limited or revoked to prevent abuse.</p>
    <h2>Availability and control</h2><p>Alert delivery depends on your device, network and messaging platform. Messages may be delayed or missed. Don’t rely on Milo for emergency or safety-critical notifications. You can turn forwarding off at any time.</p>
    <h2>Software and support</h2><p>The CLI remains available under its <a href="https://github.com/mcc0x/milo/blob/main/LICENSE">open-source license</a>. For help or to report misuse, contact the maintainers through the <a href="https://github.com/mcc0x/milo/issues">Milo project</a>. Never include credentials or private messages in a public report.</p>
  </article>;
}
