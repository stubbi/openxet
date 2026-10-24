"use client";

import { useEffect, useState } from "react";

export function RemovalForm() {
  const [receipt, setReceipt] = useState("");
  useEffect(() => {
    const value = window.location.hash.slice(1);
    if (/^[A-Za-z0-9_-]{43}$/.test(value)) setReceipt(value);
    if (window.location.hash) window.history.replaceState(null, "", window.location.pathname);
  }, []);

  return (
    <form action="/api/waitlist/cancel" method="post">
      <label htmlFor="receipt">Private removal code</label>
      <input id="receipt" name="receipt" type="text" value={receipt} onChange={event => setReceipt(event.target.value)} autoComplete="off" spellCheck={false} minLength={43} maxLength={43} pattern="[A-Za-z0-9_-]{43}" required aria-describedby="receipt-help" />
      <p className="small" id="receipt-help">Your private link fills this in. With JavaScript off, paste the code after the # in that link.</p>
      <button type="submit">Remove my request</button>
    </form>
  );
}
