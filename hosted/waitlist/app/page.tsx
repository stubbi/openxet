export default function Home() {
  return (
    <main className="panel">
      <a className="brand" href="https://openxet.com/">OpenXet</a>
      <p className="eyebrow">Hosted storage · coming soon</p>
      <h1>Be here for the first upload.</h1>
      <p>Join the waitlist for hosted OpenXet. We’ll contact you when access is ready.</p>
      <form action="/api/waitlist" method="post">
        <label htmlFor="email">Email address</label>
        <input id="email" name="email" type="email" inputMode="email" autoComplete="email" maxLength={254} required aria-describedby="email-use" placeholder="you@example.com" />
        <div className="trap" aria-hidden="true">
          <label htmlFor="website">Leave this field empty</label>
          <input id="website" name="website" type="text" autoComplete="off" tabIndex={-1} />
        </div>
        <p className="small" id="email-use">Your address is used for launch access only. No account is created. <a href="https://openxet.com/privacy/">How we use your email</a></p>
        <button type="submit">Join the waitlist <span aria-hidden="true">→</span></button>
      </form>
      <p className="small">Prefer to run it yourself? <a href="https://openxet.com/docs/quickstart-storage-only.html">Try the storage server</a>.</p>
    </main>
  );
}
