import { waitlistRequest } from "../../../lib/waitlist-handler";

export const dynamic = "force-dynamic";
export const POST = (request: Request) => waitlistRequest(request);
export const OPTIONS = POST;
export const GET = POST;
export const PUT = POST;
export const PATCH = POST;
export const DELETE = POST;
