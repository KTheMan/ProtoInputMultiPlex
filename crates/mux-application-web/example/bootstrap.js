import init, { mount_demo } from './pkg/mux_application_web.js';
await init();
const client = mount_demo('application-demo');
export { client };
